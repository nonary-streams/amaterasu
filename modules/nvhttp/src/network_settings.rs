//! Reconciles the persisted NVHTTP base port with the RTSP and UDP endpoint owners.
use crate::product_settings::ProductSettings;
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const RTSP_SERVICE: &str = "amaterasu.rtsp/1";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

pub struct NetworkSettings {
    active_base_port: Option<u16>,
    error: Option<String>,
    pending: Option<PendingCall>,
    sequence: u64,
    next_retry: Instant,
    retry_delay: Duration,
}

struct PendingCall {
    operation_id: String,
    port: u16,
    parent: Option<u64>,
    state: CallState,
}

enum CallState {
    Submitting,
    Waiting { child: u64 },
    Releasing { child: u64, outcome: ChildOutcome },
}

enum ChildOutcome {
    Payload(Value),
    Failure { code: String, message: String },
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            active_base_port: None,
            error: None,
            pending: None,
            sequence: 0,
            next_retry: Instant::now(),
            retry_delay: INITIAL_RETRY_DELAY,
        }
    }
}

impl NetworkSettings {
    pub fn active_base_port(&self) -> Option<u16> {
        self.active_base_port
    }

    /// Advances one idempotent RTSP configuration call, then starts a retry when needed.
    pub fn tick(
        &mut self,
        runtime: &RuntimeClient,
        port: u16,
        settings: &mut ProductSettings,
    ) -> ModuleResult<()> {
        if let Some(pending) = self.pending.take()
            && !self.advance(pending, runtime, settings)?
        {
            return Ok(());
        }
        if self.active_base_port == Some(port) || Instant::now() < self.next_retry {
            settings.set_active_rtsp_base_port(self.active_base_port);
            return Ok(());
        }
        let Some(sequence) = self.sequence.checked_add(1) else {
            self.record_failure(
                port,
                "resource_exhausted".into(),
                "network configuration operation identifier exhausted".into(),
                settings,
            );
            return Ok(());
        };
        self.sequence = sequence;
        self.pending = Some(PendingCall {
            operation_id: format!("nvhttp.network_settings.{sequence}"),
            port,
            parent: settings.rtsp_completion_request(port),
            state: CallState::Submitting,
        });
        Ok(())
    }

    fn advance(
        &mut self,
        mut pending: PendingCall,
        runtime: &RuntimeClient,
        settings: &mut ProductSettings,
    ) -> ModuleResult<bool> {
        match &mut pending.state {
            CallState::Submitting => match runtime.send(Command::Request {
                service: RTSP_SERVICE.into(),
                payload: json!({"action":"network_configure","port":pending.port}),
                timeout_ms: REQUEST_TIMEOUT.as_millis() as u64,
                operation_id: pending.operation_id.clone(),
                parent: pending.parent,
            }) {
                Ok(Response::Accepted { id }) => {
                    pending.state = CallState::Waiting { child: id };
                    self.pending = Some(pending);
                    return Ok(false);
                }
                Ok(Response::Error { code, message }) => {
                    self.record_failure(pending.port, code, message, settings);
                    return Ok(true);
                }
                Ok(response) => {
                    self.record_failure(
                        pending.port,
                        "invalid_response".into(),
                        format!("RTSP configuration request returned {response:?}"),
                        settings,
                    );
                    return Ok(true);
                }
                Err(error) => {
                    self.error = Some(error.clone());
                    pending.state = CallState::Submitting;
                    self.pending = Some(pending);
                    return Ok(false);
                }
            },
            CallState::Waiting { child } => {
                match runtime.send(Command::TakeResult { request: *child }) {
                    Ok(Response::Pending) => {
                        self.pending = Some(pending);
                        return Ok(false);
                    }
                    Ok(Response::Result { payload }) => {
                        pending.state = CallState::Releasing {
                            child: *child,
                            outcome: ChildOutcome::Payload(payload),
                        };
                    }
                    Ok(Response::Error { code, message }) if code == "not_found" => {
                        self.record_failure(pending.port, code, message, settings);
                        return Ok(true);
                    }
                    Ok(Response::Error { code, message }) => {
                        pending.state = CallState::Releasing {
                            child: *child,
                            outcome: ChildOutcome::Failure { code, message },
                        };
                    }
                    Ok(response) => {
                        pending.state = CallState::Releasing {
                            child: *child,
                            outcome: ChildOutcome::Failure {
                                code: "invalid_response".into(),
                                message: format!("RTSP configuration result returned {response:?}"),
                            },
                        };
                    }
                    Err(error) => {
                        self.error = Some(error);
                        self.pending = Some(pending);
                        return Ok(false);
                    }
                }
            }
            CallState::Releasing { .. } => {}
        }

        let CallState::Releasing { child, outcome } =
            std::mem::replace(&mut pending.state, CallState::Submitting)
        else {
            unreachable!("terminal child result must be released")
        };
        match runtime.send(Command::ReleaseResult { request: child }) {
            Ok(Response::Ok) => {
                self.finish(pending.port, outcome, settings);
                Ok(true)
            }
            Ok(Response::Error { code, .. }) if code == "not_found" => {
                self.finish(pending.port, outcome, settings);
                Ok(true)
            }
            Ok(Response::Error { code, message }) => {
                self.error = Some(format!("RTSP result release failed: {code}: {message}"));
                pending.state = CallState::Releasing { child, outcome };
                self.pending = Some(pending);
                Ok(false)
            }
            Ok(response) => {
                self.error = Some(format!("RTSP result release returned {response:?}"));
                pending.state = CallState::Releasing { child, outcome };
                self.pending = Some(pending);
                Ok(false)
            }
            Err(error) => {
                self.error = Some(error);
                pending.state = CallState::Releasing { child, outcome };
                self.pending = Some(pending);
                Ok(false)
            }
        }
    }

    fn finish(&mut self, port: u16, outcome: ChildOutcome, settings: &mut ProductSettings) {
        match outcome {
            ChildOutcome::Payload(payload) => {
                let reported_active_port = payload["active_base_port"]
                    .as_u64()
                    .and_then(|value| u16::try_from(value).ok());
                let fully_applied = payload["ok"] == true
                    && payload["desired_port"].as_u64() == Some(u64::from(port))
                    && payload["active"] == true
                    && payload["udp_binds_preflighted"] == true
                    && reported_active_port == Some(port);
                if fully_applied {
                    self.active_base_port = reported_active_port;
                    self.error = None;
                    self.retry_delay = INITIAL_RETRY_DELAY;
                    self.next_retry = Instant::now() + self.retry_delay;
                    settings.set_active_rtsp_base_port(reported_active_port);
                    settings.record_rtsp_apply_success();
                } else {
                    if payload["active"] == false && payload["udp_binds_preflighted"] == false {
                        self.active_base_port = reported_active_port.or(self.active_base_port);
                    }
                    let message = payload["error"]
                        .as_str()
                        .or_else(|| {
                            payload["pending"]
                                .as_bool()
                                .filter(|pending| *pending)
                                .map(|_| "an RTSP stream is active")
                        })
                        .unwrap_or("RTSP did not activate the requested base port")
                        .to_owned();
                    self.record_failure(port, "listener_apply_pending".into(), message, settings);
                    settings.set_active_rtsp_base_port(self.active_base_port);
                }
            }
            ChildOutcome::Failure { code, message } => {
                self.record_failure(port, code, message, settings);
            }
        }
    }

    fn record_failure(
        &mut self,
        port: u16,
        code: String,
        message: String,
        settings: &mut ProductSettings,
    ) {
        let description = format!("{code}: {message}");
        self.error = Some(description.clone());
        settings.record_rtsp_apply_failure(port, description);
        self.next_retry = Instant::now() + self.retry_delay;
        self.retry_delay = (self.retry_delay * 2).min(MAX_RETRY_DELAY);
    }
}

#[cfg(test)]
mod tests;
