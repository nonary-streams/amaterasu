//! Reconciles the one fixed synthetic Fullscreen row from durable product settings.

use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const CATALOG_SERVICE: &str = "amaterasu.catalog/1";
const REQUEST_TIMEOUT_MS: u64 = 30_000;
const RETRY_DELAY: Duration = Duration::from_secs(3);

enum Call {
    Submit {
        enabled: bool,
        operation_id: String,
    },
    Waiting {
        enabled: bool,
        request: u64,
    },
    Release {
        enabled: bool,
        request: u64,
        payload: Option<Value>,
        failure: Option<String>,
    },
}

#[derive(Default)]
pub struct FullscreenEntrySync {
    desired: Option<bool>,
    call: Option<Call>,
    retry_at: Option<Instant>,
    sequence: u64,
    last_error: Option<String>,
}

impl FullscreenEntrySync {
    pub fn set_desired(&mut self, enabled: bool) {
        if self.desired != Some(enabled) {
            self.desired = Some(enabled);
            self.retry_at = None;
            self.last_error = None;
        }
    }

    pub fn advance(&mut self, runtime: &RuntimeClient) -> ModuleResult<()> {
        let now = Instant::now();
        if self.call.is_none() {
            let Some(enabled) = self.desired else {
                return Ok(());
            };
            if self.retry_at.is_some_and(|retry_at| now < retry_at) {
                return Ok(());
            }
            self.sequence = self.sequence.saturating_add(1);
            self.call = Some(Call::Submit {
                enabled,
                operation_id: format!("playnite.fullscreen-entry.{}", self.sequence),
            });
        }

        let Some(call) = self.call.take() else {
            return Ok(());
        };
        match call {
            Call::Submit {
                enabled,
                operation_id,
            } => match runtime.send(Command::Request {
                service: CATALOG_SERVICE.into(),
                payload: json!({
                    "action":"provider_playnite_fullscreen_entry",
                    "enabled":enabled,
                }),
                timeout_ms: REQUEST_TIMEOUT_MS,
                operation_id: operation_id.clone(),
                parent: None,
            }) {
                Ok(Response::Accepted { id }) => {
                    self.call = Some(Call::Waiting {
                        enabled,
                        request: id,
                    });
                    Ok(())
                }
                Ok(Response::Result { payload }) => self.consume(enabled, payload),
                Ok(Response::Error { code, message }) => {
                    self.retry(enabled, format!("{code}: {message}"));
                    Ok(())
                }
                Ok(response) => {
                    self.retry(
                        enabled,
                        format!("unexpected catalog response: {response:?}"),
                    );
                    Ok(())
                }
                Err(_) => {
                    self.call = Some(Call::Submit {
                        enabled,
                        operation_id,
                    });
                    Ok(())
                }
            },
            Call::Waiting { enabled, request } => {
                match runtime.send_ephemeral(Command::TakeResult { request }) {
                    Ok(Response::Pending) => {
                        self.call = Some(Call::Waiting { enabled, request });
                        Ok(())
                    }
                    Ok(Response::Result { payload }) => {
                        self.call = Some(Call::Release {
                            enabled,
                            request,
                            payload: Some(payload),
                            failure: None,
                        });
                        Ok(())
                    }
                    Ok(Response::Error { code, message }) => {
                        self.call = Some(Call::Release {
                            enabled,
                            request,
                            payload: None,
                            failure: Some(format!("{code}: {message}")),
                        });
                        Ok(())
                    }
                    Ok(response) => {
                        self.call = Some(Call::Release {
                            enabled,
                            request,
                            payload: None,
                            failure: Some(format!("unexpected catalog result: {response:?}")),
                        });
                        Ok(())
                    }
                    Err(_) => {
                        self.call = Some(Call::Waiting { enabled, request });
                        Ok(())
                    }
                }
            }
            Call::Release {
                enabled,
                request,
                payload,
                failure,
            } => match runtime.send_ephemeral(Command::ReleaseResult { request }) {
                Ok(Response::Ok) => match payload {
                    Some(payload) => self.consume(enabled, payload),
                    None => {
                        self.retry(
                            enabled,
                            failure.unwrap_or_else(|| "catalog result disappeared".into()),
                        );
                        Ok(())
                    }
                },
                Ok(Response::Error { code, .. }) if code == "not_found" => match payload {
                    Some(payload) => self.consume(enabled, payload),
                    None => {
                        self.retry(
                            enabled,
                            failure.unwrap_or_else(|| "catalog result disappeared".into()),
                        );
                        Ok(())
                    }
                },
                Err(_) | Ok(Response::Error { .. }) => {
                    self.call = Some(Call::Release {
                        enabled,
                        request,
                        payload,
                        failure,
                    });
                    Ok(())
                }
                Ok(response) => {
                    self.call = Some(Call::Release {
                        enabled,
                        request,
                        payload,
                        failure,
                    });
                    let _ = response;
                    Ok(())
                }
            },
        }
    }

    pub fn status(&self) -> Value {
        json!({
            "desired":self.desired,
            "pending":self.call.is_some(),
            "last_error":self.last_error.as_deref(),
        })
    }

    fn consume(&mut self, requested: bool, payload: Value) -> ModuleResult<()> {
        if valid_success(&payload, requested) {
            self.last_error = None;
            self.retry_at = None;
        } else {
            let error = payload
                .pointer("/error/code")
                .and_then(Value::as_str)
                .or_else(|| payload.get("code").and_then(Value::as_str))
                .unwrap_or("invalid_catalog_response");
            self.retry(requested, error.into());
        }
        Ok(())
    }

    fn retry(&mut self, requested: bool, error: String) {
        if self.desired == Some(requested) {
            self.retry_at = Some(Instant::now() + RETRY_DELAY);
            self.last_error = Some(error);
        }
    }
}

fn valid_success(payload: &Value, enabled: bool) -> bool {
    let Some(object) = payload.as_object() else {
        return false;
    };
    object.len() == 4
        && object
            .keys()
            .all(|key| matches!(key.as_str(), "ok" | "enabled" | "changed" | "revision"))
        && payload["ok"] == true
        && payload["enabled"] == enabled
        && payload["changed"].is_boolean()
        && payload["revision"].as_u64().is_some()
}

#[cfg(test)]
mod tests {
    use super::{FullscreenEntrySync, valid_success};
    use serde_json::json;

    #[test]
    fn success_response_is_exact_and_confirms_the_requested_state() {
        for enabled in [true, false] {
            assert!(valid_success(
                &json!({"ok":true,"enabled":enabled,"changed":true,"revision":3}),
                enabled,
            ));
            assert!(!valid_success(
                &json!({"ok":true,"enabled":!enabled,"changed":true,"revision":3}),
                enabled,
            ));
        }
        for malformed in [
            json!({"ok":true,"enabled":true,"revision":3}),
            json!({"ok":true,"enabled":true,"changed":true,"revision":3,"owner":"attacker"}),
            json!({"ok":true,"enabled":true,"changed":true,"revision":"3"}),
        ] {
            assert!(!valid_success(&malformed, true), "accepted {malformed}");
        }
    }

    #[test]
    fn setting_changes_coalesce_to_the_latest_desired_state() {
        let mut sync = FullscreenEntrySync::default();
        sync.set_desired(true);
        assert_eq!(sync.status()["desired"], true);
        sync.set_desired(false);
        assert_eq!(sync.status()["desired"], false);
        assert_eq!(sync.status()["pending"], false);
    }
}
