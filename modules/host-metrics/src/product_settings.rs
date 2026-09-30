//! Revisioned settings stored in this module's `amaterasu.configuration/1` document.
use crate::model::Settings;
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::collections::VecDeque;

const CONFIGURATION: &str = "amaterasu.configuration/1";
const TIMEOUT_MS: u64 = 30_000;

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
    Release { request: u64, payload: Value },
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
                operation_id: format!("host-metrics.configuration.register.{startup_id}"),
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
                "host metrics settings are not loaded",
            ));
        }
        if self.pending.is_some() {
            return Err(failure(
                "settings_busy",
                "another host metrics settings operation is pending",
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
            "host-metrics.configuration.update.{}.{}",
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
                timeout_ms: TIMEOUT_MS,
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
                    pending.state = CallState::Release { request, payload };
                }
                Ok(Response::Error { code, message }) => {
                    self.finish_error(pending.purpose.clone(), &code, &message);
                    finished = true;
                }
                Ok(_) => pending.state = CallState::Waiting(request),
            },
            CallState::Release { request, payload } => {
                match send(Command::ReleaseResult { request }) {
                    Ok(Response::Ok) => {
                        self.finish_payload(pending.purpose.clone(), payload.clone());
                        finished = true;
                    }
                    Ok(Response::Error { code, .. }) if code == "not_found" => {
                        self.finish_payload(pending.purpose.clone(), payload.clone());
                        finished = true;
                    }
                    Ok(_) | Err(_) => pending.state = CallState::Release { request, payload },
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
                self.pending = Some(PendingCall {
                    purpose: Purpose::Load,
                    payload: json!({"action":"get"}),
                    operation_id: self.next_id("load"),
                    parent: None,
                    state: CallState::Submit,
                });
            }
            Purpose::Load => {
                if let Some((settings, revision)) = parse_settings_document(&payload) {
                    self.settings = settings;
                    self.revision = revision;
                    self.ready = true;
                } else {
                    self.pending = Some(PendingCall {
                        purpose: Purpose::Load,
                        payload: json!({"action":"get"}),
                        operation_id: self.next_id("reload"),
                        parent: None,
                        state: CallState::Submit,
                    });
                }
            }
            Purpose::Update {
                management_request,
                expected_revision,
                desired,
            } => {
                let success = parse_settings_document(&payload);
                if let Some((settings, revision)) = success
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
                    self.pending = Some(PendingCall {
                        purpose: Purpose::Load,
                        payload: json!({"action":"get"}),
                        operation_id: self.next_id("reload"),
                        parent: None,
                        state: CallState::Submit,
                    });
                }
            }
        }
    }

    fn finish_error(&mut self, purpose: Purpose, code: &str, message: &str) {
        match purpose {
            Purpose::Register | Purpose::Load => {
                self.ready = false;
                self.pending = Some(PendingCall {
                    purpose: Purpose::Register,
                    payload: register_payload(),
                    operation_id: self.next_id("register-retry"),
                    parent: None,
                    state: CallState::Submit,
                });
            }
            Purpose::Update {
                management_request, ..
            } => self.completions.push_back(Completion::Update {
                request: management_request,
                response: failure(code, message),
            }),
        }
    }

    fn next_id(&mut self, label: &str) -> String {
        self.sequence = self.sequence.saturating_add(1);
        format!(
            "host-metrics.configuration.{label}.{}.{}",
            self.generation, self.sequence
        )
    }
}

fn register_payload() -> Value {
    let boolean = |default| json!({"type":"boolean","default":default});
    let integer = |default, minimum, maximum| {
        json!({
            "type":"integer","default":default,"minimum":minimum,"maximum":maximum
        })
    };
    json!({
        "action":"register",
        "fields":{
            "enabled":boolean(true),
            "poll_interval_ms":integer(2000,250,60000),
            "history_retention_seconds":integer(300,30,3600),
            "max_history_points":integer(300,30,2000),
            "pause_when_hidden":boolean(true),
            "show_active_sessions":boolean(true),
            "show_host_stats":boolean(true),
            "show_host_charts":boolean(true),
            "show_session_history":boolean(true)
        }
    })
}

fn settings_value(settings: &Settings) -> Value {
    json!({
        "enabled":settings.enabled,
        "poll_interval_ms":settings.poll_interval_ms,
        "history_retention_seconds":settings.history_retention_seconds,
        "max_history_points":settings.max_history_points,
        "pause_when_hidden":settings.pause_when_hidden,
        "show_active_sessions":settings.show_active_sessions,
        "show_host_stats":settings.show_host_stats,
        "show_host_charts":settings.show_host_charts,
        "show_session_history":settings.show_session_history
    })
}

fn parse_settings_document(payload: &Value) -> Option<(Settings, u64)> {
    if !payload.get("ok")?.as_bool()? {
        return None;
    }
    let revision = payload.get("revision")?.as_u64()?;
    let values = payload.get("values")?.as_object()?;
    let settings = Settings {
        enabled: values.get("enabled")?.as_bool()?,
        poll_interval_ms: u32::try_from(values.get("poll_interval_ms")?.as_u64()?).ok()?,
        history_retention_seconds: u32::try_from(
            values.get("history_retention_seconds")?.as_u64()?,
        )
        .ok()?,
        max_history_points: u32::try_from(values.get("max_history_points")?.as_u64()?).ok()?,
        pause_when_hidden: values.get("pause_when_hidden")?.as_bool()?,
        show_active_sessions: values.get("show_active_sessions")?.as_bool()?,
        show_host_stats: values.get("show_host_stats")?.as_bool()?,
        show_host_charts: values.get("show_host_charts")?.as_bool()?,
        show_session_history: values.get("show_session_history")?.as_bool()?,
    };
    settings.validate().ok()?;
    Some((settings, revision))
}

fn configuration_error(payload: &Value) -> Value {
    let error = payload.get("error");
    let code = payload
        .get("code")
        .or_else(|| error.and_then(|error| error.get("code")))
        .and_then(Value::as_str)
        .unwrap_or("configuration_failed");
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("configuration service rejected the host metrics update");
    json!({"ok":false,"code":code,"message":message})
}

fn failure(code: &str, message: &str) -> Value {
    json!({"ok":false,"code":code,"message":message})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_registration_matches_the_complete_settings_schema() {
        let payload = register_payload();
        let fields = payload["fields"].as_object().unwrap();
        assert_eq!(fields.len(), 9);
        assert_eq!(fields["poll_interval_ms"]["minimum"], 250);
        assert_eq!(fields["poll_interval_ms"]["maximum"], 60_000);
        assert_eq!(fields["history_retention_seconds"]["minimum"], 30);
        assert_eq!(fields["history_retention_seconds"]["maximum"], 3_600);
        assert_eq!(fields["max_history_points"]["minimum"], 30);
        assert_eq!(fields["max_history_points"]["maximum"], 2_000);
    }

    #[test]
    fn persisted_configuration_is_rejected_when_incomplete_or_out_of_bounds() {
        let mut values = settings_value(&Settings::default());
        let mut document = json!({"ok":true,"revision":4,"values":values});
        assert_eq!(parse_settings_document(&document).unwrap().1, 4);

        values["poll_interval_ms"] = json!(249);
        document["values"] = values;
        assert!(parse_settings_document(&document).is_none());

        document["values"] = json!({"enabled":true});
        assert!(parse_settings_document(&document).is_none());
    }

    #[test]
    fn completed_update_replies_are_not_success_without_revision_and_value_readback() {
        let error = configuration_error(&json!({
            "ok":false,
            "error":{"code":"revision_conflict","message":"revision changed","current_revision":8}
        }));
        assert_eq!(error["code"], "revision_conflict");
        assert_eq!(error["message"], "revision changed");
        assert!(parse_settings_document(&json!({"ok":true,"revision":2,"values":{}})).is_none());
    }
}
