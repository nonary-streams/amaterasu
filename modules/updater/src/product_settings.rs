//! Durable updater preferences held in the updater principal's configuration document.
use crate::model::{MAX_UPDATE_CHECK_INTERVAL_SECONDS, Settings};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::collections::VecDeque;

const CONFIGURATION: &str = "amaterasu.configuration/1";
const CALL_TIMEOUT_MS: u64 = 30_000;

#[derive(Clone)]
enum Purpose {
    Register,
    Load,
    Update {
        management_request: u64,
        expected_revision: u64,
        desired: Settings,
    },
}

enum CallState {
    Submit,
    Waiting(u64),
    Release {
        request: u64,
        outcome: Result<Value, (String, String)>,
    },
}

struct PendingCall {
    purpose: Purpose,
    payload: Value,
    operation_id: String,
    parent: Option<u64>,
    state: CallState,
}

pub enum Completion {
    Update { request: u64, response: Value },
}

pub struct ProductSettings {
    generation: String,
    settings: Settings,
    revision: u64,
    ready: bool,
    sequence: u64,
    pending: Option<PendingCall>,
    completions: VecDeque<Completion>,
}

impl ProductSettings {
    pub fn new(startup_id: &str) -> Self {
        Self {
            generation: startup_id.to_owned(),
            settings: Settings::default(),
            revision: 0,
            ready: false,
            sequence: 0,
            pending: Some(PendingCall {
                purpose: Purpose::Register,
                payload: register_payload(),
                operation_id: format!("updater.configuration.register.{startup_id}"),
                parent: None,
                state: CallState::Submit,
            }),
            completions: VecDeque::new(),
        }
    }

    pub fn settings(&self) -> (&Settings, u64) {
        (&self.settings, self.revision)
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn begin_update(
        &mut self,
        request: u64,
        expected_revision: u64,
        desired: Settings,
    ) -> Result<(), Value> {
        if !self.ready {
            return Err(failure(
                "configuration_unavailable",
                "updater settings are not loaded",
            ));
        }
        if self.pending.is_some() {
            return Err(failure(
                "settings_busy",
                "another updater settings operation is pending",
            ));
        }
        desired
            .validate()
            .map_err(|message| failure("invalid_request", message))?;
        if expected_revision != self.revision {
            return Err(json!({
                "ok":false,
                "code":"revision_conflict",
                "current_revision":self.revision
            }));
        }

        self.sequence = self.sequence.saturating_add(1);
        let operation_id = format!(
            "updater.configuration.update.{}.{}",
            self.generation, self.sequence
        );
        self.pending = Some(PendingCall {
            purpose: Purpose::Update {
                management_request: request,
                expected_revision,
                desired: desired.clone(),
            },
            payload: json!({
                "action":"update",
                "expected_revision":expected_revision,
                "values":settings_value(&desired)
            }),
            operation_id,
            parent: Some(request),
            state: CallState::Submit,
        });
        Ok(())
    }

    pub fn take_completion(&mut self) -> Option<Completion> {
        self.completions.pop_front()
    }

    pub fn advance(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        let Some(mut pending) = self.pending.take() else {
            return Ok(());
        };
        let mut finished = false;
        let state = std::mem::replace(&mut pending.state, CallState::Submit);
        match state {
            CallState::Submit => match send(Command::Request {
                service: CONFIGURATION.into(),
                payload: pending.payload.clone(),
                timeout_ms: CALL_TIMEOUT_MS,
                operation_id: pending.operation_id.clone(),
                parent: pending.parent,
            }) {
                Ok(Response::Accepted { id }) => pending.state = CallState::Waiting(id),
                Ok(Response::Error { code, message }) => {
                    self.finish_error(pending.purpose.clone(), &code, &message);
                    finished = true;
                }
                Ok(_) | Err(_) => pending.state = CallState::Submit,
            },
            CallState::Waiting(request) => match send(Command::TakeResult { request }) {
                Ok(Response::Pending) | Err(_) => pending.state = CallState::Waiting(request),
                Ok(Response::Result { payload }) => {
                    pending.state = CallState::Release {
                        request,
                        outcome: Ok(payload),
                    };
                }
                Ok(Response::Error { code, message }) if code == "not_found" => {
                    self.finish_error(pending.purpose.clone(), &code, &message);
                    finished = true;
                }
                Ok(Response::Error { code, message }) => {
                    pending.state = CallState::Release {
                        request,
                        outcome: Err((code, message)),
                    };
                }
                Ok(_) => {
                    pending.state = CallState::Release {
                        request,
                        outcome: Err((
                            "invalid_response".into(),
                            "configuration returned an unexpected response".into(),
                        )),
                    };
                }
            },
            CallState::Release { request, outcome } => {
                match send(Command::ReleaseResult { request }) {
                    Ok(Response::Ok) => {
                        self.finish_outcome(pending.purpose.clone(), outcome);
                        finished = true;
                    }
                    Ok(Response::Error { code, .. }) if code == "not_found" => {
                        self.finish_outcome(pending.purpose.clone(), outcome);
                        finished = true;
                    }
                    Ok(_) | Err(_) => pending.state = CallState::Release { request, outcome },
                }
            }
        }
        if !finished {
            self.pending = Some(pending);
        }
        Ok(())
    }

    fn finish_payload(&mut self, purpose: Purpose, payload: Value) {
        match purpose {
            Purpose::Register => {
                if let Some((settings, revision)) = parse_document(&payload) {
                    self.settings = settings;
                    self.revision = revision;
                    self.ready = true;
                } else {
                    self.schedule_load();
                }
            }
            Purpose::Load => {
                if let Some((settings, revision)) = parse_document(&payload) {
                    self.settings = settings;
                    self.revision = revision;
                    self.ready = true;
                } else {
                    self.ready = false;
                    self.schedule_register();
                }
            }
            Purpose::Update {
                management_request,
                expected_revision,
                desired,
            } => {
                if let Some((settings, revision)) = parse_document(&payload)
                    && revision == expected_revision.saturating_add(1)
                    && settings == desired
                {
                    self.settings = settings.clone();
                    self.revision = revision;
                    self.completions.push_back(Completion::Update {
                        request: management_request,
                        response: json!({"ok":true,"settings":settings,"revision":revision}),
                    });
                } else {
                    let response = configuration_error(&payload);
                    self.completions.push_back(Completion::Update {
                        request: management_request,
                        response,
                    });
                    self.schedule_load();
                }
            }
        }
    }

    fn finish_outcome(&mut self, purpose: Purpose, outcome: Result<Value, (String, String)>) {
        match outcome {
            Ok(payload) => self.finish_payload(purpose, payload),
            Err((code, message)) => self.finish_error(purpose, &code, &message),
        }
    }

    fn finish_error(&mut self, purpose: Purpose, code: &str, message: &str) {
        match purpose {
            Purpose::Register | Purpose::Load => {
                self.ready = false;
                self.schedule_register();
            }
            Purpose::Update {
                management_request, ..
            } => {
                self.completions.push_back(Completion::Update {
                    request: management_request,
                    response: failure(code, message),
                });
                self.schedule_load();
            }
        }
    }

    fn schedule_load(&mut self) {
        self.pending = Some(PendingCall {
            purpose: Purpose::Load,
            payload: json!({"action":"get"}),
            operation_id: self.next_id("load"),
            parent: None,
            state: CallState::Submit,
        });
    }

    fn schedule_register(&mut self) {
        self.pending = Some(PendingCall {
            purpose: Purpose::Register,
            payload: register_payload(),
            operation_id: self.next_id("register-retry"),
            parent: None,
            state: CallState::Submit,
        });
    }

    fn next_id(&mut self, name: &str) -> String {
        self.sequence = self.sequence.saturating_add(1);
        format!(
            "updater.configuration.{name}.{}.{}",
            self.generation, self.sequence
        )
    }
}

pub fn register_payload() -> Value {
    json!({
        "action":"register",
        "fields":{
            "notify_pre_releases":{"type":"boolean","default":false},
            "update_check_interval":{
                "type":"integer",
                "default":86_400,
                "minimum":0,
                "maximum":MAX_UPDATE_CHECK_INTERVAL_SECONDS
            }
        }
    })
}

fn settings_value(settings: &Settings) -> Value {
    json!({
        "notify_pre_releases":settings.notify_pre_releases,
        "update_check_interval":settings.update_check_interval
    })
}

fn parse_document(payload: &Value) -> Option<(Settings, u64)> {
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let revision = payload.get("revision")?.as_u64()?;
    let settings: Settings = serde_json::from_value(payload.get("values")?.clone()).ok()?;
    settings.validate().ok()?;
    Some((settings, revision))
}

fn configuration_error(payload: &Value) -> Value {
    let error = payload.get("error").unwrap_or(payload);
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("settings_outcome_unknown");
    if code == "revision_conflict" {
        return json!({
            "ok":false,
            "code":code,
            "current_revision":error.get("current_revision").and_then(Value::as_u64)
        });
    }
    failure(code, "updater settings update could not be confirmed")
}

fn failure(code: &str, message: &str) -> Value {
    json!({"ok":false,"code":code,"message":message})
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::Response;

    #[test]
    fn schema_defaults_match_the_two_global_update_options_and_seconds_unit() {
        let schema = register_payload();
        assert_eq!(schema["fields"]["notify_pre_releases"]["default"], false);
        assert_eq!(schema["fields"]["update_check_interval"]["default"], 86_400);
        assert_eq!(schema["fields"]["update_check_interval"]["minimum"], 0);
        assert_eq!(
            schema["fields"]["update_check_interval"]["maximum"],
            MAX_UPDATE_CHECK_INTERVAL_SECONDS
        );
    }

    #[test]
    fn persisted_settings_require_complete_valid_values_and_revision() {
        let settings = Settings::default();
        let document = json!({
            "ok":true,
            "revision":4,
            "values":settings_value(&settings)
        });
        assert_eq!(parse_document(&document), Some((settings, 4)));
        assert!(parse_document(&json!({"ok":true,"revision":0,"values":{}})).is_none());
        assert!(
            parse_document(&json!({
                "ok":true,
                "revision":0,
                "values":{"notify_pre_releases":false,"update_check_interval":-1}
            }))
            .is_none()
        );
    }

    fn advance_configuration_call(
        product_settings: &mut ProductSettings,
        expected_action: &str,
        child_request: u64,
        result: Value,
    ) {
        product_settings
            .advance(&mut |command| match command {
                Command::Request {
                    service, payload, ..
                } => {
                    assert_eq!(service, CONFIGURATION);
                    assert_eq!(payload["action"], expected_action);
                    Ok(Response::Accepted { id: child_request })
                }
                unexpected => panic!("expected configuration request, got {unexpected:?}"),
            })
            .unwrap();
        product_settings
            .advance(&mut |command| match command {
                Command::TakeResult { request } => {
                    assert_eq!(request, child_request);
                    Ok(Response::Result {
                        payload: result.clone(),
                    })
                }
                unexpected => panic!("expected configuration result, got {unexpected:?}"),
            })
            .unwrap();
        product_settings
            .advance(&mut |command| match command {
                Command::ReleaseResult { request } => {
                    assert_eq!(request, child_request);
                    Ok(Response::Ok)
                }
                unexpected => panic!("expected configuration result release, got {unexpected:?}"),
            })
            .unwrap();
    }

    #[test]
    fn settings_use_cas_and_survive_a_module_restart() {
        let mut product_settings = ProductSettings::new("first-start");
        advance_configuration_call(
            &mut product_settings,
            "register",
            10,
            json!({
                "ok":true,
                "revision":1,
                "values":settings_value(&Settings::default())
            }),
        );
        assert!(product_settings.is_ready());
        assert_eq!(product_settings.settings(), (&Settings::default(), 1));

        let desired = Settings {
            notify_pre_releases: true,
            update_check_interval: 0,
        };
        assert_eq!(
            product_settings.begin_update(44, 0, desired.clone()),
            Err(json!({"ok":false,"code":"revision_conflict","current_revision":1}))
        );
        product_settings
            .begin_update(45, 1, desired.clone())
            .unwrap();
        advance_configuration_call(
            &mut product_settings,
            "update",
            11,
            json!({
                "ok":true,
                "revision":2,
                "values":settings_value(&desired)
            }),
        );
        assert_eq!(product_settings.settings(), (&desired, 2));
        let Some(Completion::Update { request, response }) = product_settings.take_completion()
        else {
            panic!("confirmed update must complete its management request")
        };
        assert_eq!(request, 45);
        assert_eq!(response["ok"], true);
        assert_eq!(response["revision"], 2);

        let mut restarted = ProductSettings::new("second-start");
        advance_configuration_call(
            &mut restarted,
            "register",
            12,
            json!({
                "ok":true,
                "revision":2,
                "values":settings_value(&desired)
            }),
        );
        assert!(restarted.is_ready());
        assert_eq!(restarted.settings(), (&desired, 2));
        assert!(restarted.take_completion().is_none());
    }
}
