//! Product-owned, durable Playnite settings and management-only RPCs.

use crate::settings::ProductSettings;
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};

const SERVICE: &str = "amaterasu.configuration/1";
const TIMEOUT_MS: u64 = 30_000;

#[derive(Clone)]
pub struct ProductConfiguration {
    settings: ProductSettings,
    revision: u64,
    ready: bool,
    pending: Option<Pending>,
    deferred_response: Option<(u64, Value, bool)>,
}

#[derive(Clone)]
enum Operation {
    Load,
    Update {
        request: u64,
        expected_revision: u64,
        settings: ProductSettings,
    },
    Reconcile {
        request: u64,
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
}

impl ProductConfiguration {
    pub fn new() -> Self {
        Self {
            settings: ProductSettings::default(),
            revision: 0,
            ready: false,
            pending: Some(Pending {
                operation: Operation::Load,
                call: Call::Submit {
                    payload: json!({"action":"product_get"}),
                    operation_id: "playnite.product-settings.load".into(),
                },
            }),
            deferred_response: None,
        }
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
        self.pending.is_some() || self.deferred_response.is_some()
    }

    pub fn require_epoch_for_deferred_response(&mut self) {
        if let Some((_, _, required)) = self.deferred_response.as_mut() {
            *required = true;
        }
    }

    pub fn release_deferred_response(
        &mut self,
        runtime: &RuntimeClient,
        epoch_ready: bool,
    ) -> ModuleResult<()> {
        self.release_deferred_response_with(epoch_ready, |request, payload| {
            complete(runtime, request, payload)
        })
    }

    fn release_deferred_response_with(
        &mut self,
        epoch_ready: bool,
        mut send: impl FnMut(u64, Value) -> ModuleResult<()>,
    ) -> ModuleResult<()> {
        let Some((request, payload, required)) = self.deferred_response.take() else {
            return Ok(());
        };
        if required && !epoch_ready {
            self.deferred_response = Some((request, payload, required));
            return Ok(());
        }
        if let Err(error) = send(request, payload.clone()) {
            self.deferred_response = Some((request, payload, required));
            return Err(error);
        }
        Ok(())
    }
    pub fn management_response(&self) -> Value {
        self.settings.management_response(self.revision)
    }

    pub fn begin_update(
        &mut self,
        request: u64,
        settings: ProductSettings,
        expected_revision: u64,
    ) -> Result<(), Value> {
        if !self.ready {
            return Err(json!({"ok":false,"code":"settings_loading"}));
        }
        if self.busy() {
            return Err(json!({"ok":false,"code":"settings_busy"}));
        }
        if expected_revision != self.revision {
            return Err(
                json!({"ok":false,"error":{"code":"revision_conflict","current_revision":self.revision}}),
            );
        }
        settings.validate().map_err(|message| {
            json!({
                "ok":false,"error":{"code":"invalid_request","message":message}
            })
        })?;
        self.pending = Some(Pending {
            operation: Operation::Update {
                request,
                expected_revision,
                settings: settings.clone(),
            },
            call: Call::Submit {
                payload: settings.product_update_payload(expected_revision),
                operation_id: format!("playnite.product-settings.update.{request}"),
            },
        });
        Ok(())
    }

    pub fn advance(
        &mut self,
        runtime: &RuntimeClient,
        settings_epoch: &mut u64,
    ) -> ModuleResult<()> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        match pending.call {
            Call::Delay { ticks } if ticks > 0 => {
                self.pending = Some(Pending {
                    call: Call::Delay { ticks: ticks - 1 },
                    ..pending
                });
                Ok(())
            }
            Call::Delay { .. } => {
                self.pending = Some(Pending {
                    call: Call::Submit {
                        payload: json!({"action":"product_get"}),
                        operation_id: "playnite.product-settings.load.retry".into(),
                    },
                    ..pending
                });
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
                        self.pending = Some(Pending {
                            call: Call::Waiting { request: id },
                            ..pending
                        });
                        Ok(())
                    }
                    Ok(response @ Response::Result { .. }) => {
                        self.consume(runtime, pending.operation, response, settings_epoch)
                    }
                    Ok(Response::Error { code, message }) => {
                        self.submission_failed(pending.operation, code, message)
                    }
                    Ok(response) => {
                        self.consume(runtime, pending.operation, response, settings_epoch)
                    }
                    Err(_) => {
                        self.pending = Some(Pending {
                            call: retry,
                            operation: pending.operation,
                        });
                        Ok(())
                    }
                }
            }
            Call::Waiting { request } => match runtime
                .send_ephemeral(Command::TakeResult { request })
            {
                Ok(Response::Pending) => {
                    self.pending = Some(Pending {
                        call: Call::Waiting { request },
                        ..pending
                    });
                    Ok(())
                }
                Ok(response @ Response::Result { .. }) => {
                    self.consume(runtime, pending.operation, response, settings_epoch)
                }
                Ok(Response::Error { code, message }) => {
                    self.submission_failed(pending.operation, code, message)
                }
                Ok(response) => self.consume(runtime, pending.operation, response, settings_epoch),
                Err(_) => {
                    self.pending = Some(Pending {
                        call: Call::Waiting { request },
                        ..pending
                    });
                    Ok(())
                }
            },
        }
    }

    fn consume(
        &mut self,
        _runtime: &RuntimeClient,
        operation: Operation,
        response: Response,
        settings_epoch: &mut u64,
    ) -> ModuleResult<()> {
        let Response::Result { payload } = response else {
            return self.submission_failed(
                operation,
                "unexpected_response".into(),
                "configuration service returned an unexpected response".into(),
            );
        };
        match operation {
            Operation::Load => {
                if self.consume_document(payload, settings_epoch).is_err() {
                    self.pending = Some(Pending {
                        operation: Operation::Load,
                        call: Call::Delay { ticks: 32 },
                    });
                }
                Ok(())
            }
            Operation::Update {
                request,
                expected_revision,
                settings,
            } => {
                if payload.get("ok").and_then(Value::as_bool) == Some(true) {
                    match ProductSettings::from_product_document(&payload) {
                        Ok((actual, revision))
                            if expected_revision.checked_add(1) == Some(revision) && actual == settings =>
                        {
                            self.apply(actual, revision, settings_epoch);
                            self.deferred_response = Some((request, self.management_response(), false));
                            Ok(())
                        }
                        _ => self.schedule_reconciliation(request, UpdateResolution::Unknown("configuration returned a malformed or different committed document".into())),
                    }
                } else {
                    let code = payload
                        .pointer("/error/code")
                        .and_then(Value::as_str)
                        .unwrap_or("configuration_rejected");
                    if code == "revision_conflict" {
                        self.schedule_reconciliation(request, UpdateResolution::Conflict)
                    } else {
                        self.schedule_reconciliation(
                            request,
                            UpdateResolution::Unknown(format!("configuration returned {code}")),
                        )
                    }
                }
            }
            Operation::Reconcile {
                request,
                resolution,
            } => {
                let response = match self.consume_document(payload, settings_epoch) {
                    Ok(()) => match resolution {
                        UpdateResolution::Conflict => {
                            json!({"ok":false,"error":{"code":"revision_conflict","current_revision":self.revision}})
                        }
                        UpdateResolution::Unknown(reason) => {
                            json!({"ok":false,"code":"update_outcome_unknown","message":reason,"revision":self.revision})
                        }
                    },
                    Err(error) => {
                        self.pending = Some(Pending {
                            operation: Operation::Load,
                            call: Call::Delay { ticks: 32 },
                        });
                        match resolution {
                            UpdateResolution::Conflict => {
                                json!({"ok":false,"error":{"code":"revision_conflict","message":format!("configuration readback failed: {error}"),"current_revision":self.revision}})
                            }
                            UpdateResolution::Unknown(reason) => {
                                json!({"ok":false,"code":"update_outcome_unknown","message":format!("{reason}; readback failed: {error}")})
                            }
                        }
                    }
                };
                self.deferred_response = Some((request, response, false));
                Ok(())
            }
        }
    }

    fn consume_document(&mut self, payload: Value, settings_epoch: &mut u64) -> ModuleResult<()> {
        let (settings, revision) = ProductSettings::from_product_document(&payload)?;
        self.apply(settings, revision, settings_epoch);
        Ok(())
    }

    fn apply(&mut self, settings: ProductSettings, revision: u64, settings_epoch: &mut u64) {
        if self.settings != settings {
            *settings_epoch = (*settings_epoch).saturating_add(1);
        }
        self.settings = settings;
        self.revision = revision;
        self.ready = true;
    }

    fn submission_failed(
        &mut self,
        operation: Operation,
        code: String,
        message: String,
    ) -> ModuleResult<()> {
        match operation {
            Operation::Load => {
                self.pending = Some(Pending {
                    operation: Operation::Load,
                    call: Call::Delay { ticks: 32 },
                });
                Ok(())
            }
            Operation::Update { request, .. } => self.schedule_reconciliation(
                request,
                UpdateResolution::Unknown(format!("{code}: {message}")),
            ),
            Operation::Reconcile {
                request,
                resolution,
            } => {
                self.pending = Some(Pending {
                    operation: Operation::Reconcile {
                        request,
                        resolution,
                    },
                    call: Call::Delay { ticks: 32 },
                });
                Ok(())
            }
        }
    }

    fn schedule_reconciliation(
        &mut self,
        request: u64,
        resolution: UpdateResolution,
    ) -> ModuleResult<()> {
        self.pending = Some(Pending {
            operation: Operation::Reconcile {
                request,
                resolution,
            },
            call: Call::Submit {
                payload: json!({"action":"product_get"}),
                operation_id: format!("playnite.product-settings.readback.{request}"),
            },
        });
        Ok(())
    }
}

fn complete(runtime: &RuntimeClient, request: u64, payload: Value) -> ModuleResult<()> {
    match runtime.send(Command::Complete { request, payload })? {
        Response::Ok => Ok(()),
        Response::Error { code, .. } if code == "request_ended" || code == "not_found" => Ok(()),
        response => Err(format!("Playnite settings response failed: {response:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disable_response_waits_for_durable_epoch_acknowledgment() {
        let mut configuration = ProductConfiguration::new();
        configuration.pending = None;
        configuration.deferred_response = Some((71, json!({"ok":true,"revision":2}), false));
        configuration.require_epoch_for_deferred_response();
        let mut completions = Vec::new();
        configuration
            .release_deferred_response_with(false, |request, payload| {
                completions.push((request, payload));
                Ok(())
            })
            .unwrap();
        assert!(configuration.busy());
        assert!(completions.is_empty());
        configuration
            .release_deferred_response_with(true, |request, payload| {
                completions.push((request, payload));
                Ok(())
            })
            .unwrap();
        assert_eq!(completions, [(71, json!({"ok":true,"revision":2}))]);
        assert!(!configuration.busy());
    }
}
