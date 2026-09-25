//! Asynchronously reads and refreshes the persisted host logging policy.
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const HOST_CONFIGURATION_SERVICE: &str = "amaterasu.host-configuration/1";
// A single diagnostics reader per caller can reattach to this key across restarts.
const HOST_LOGGING_OPERATION_ID: &str = "diagnostics.host-configuration.logging.read";
const REQUEST_TIMEOUT_MS: u64 = 30_000;
const RETRY_DELAY: Duration = Duration::from_secs(5);
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

enum ChildOutcome {
    Payload(Value),
    Failure,
}

enum PendingPhase {
    Waiting {
        request_id: u64,
    },
    Releasing {
        request_id: u64,
        outcome: ChildOutcome,
    },
}

/// Polls the host configuration service without blocking diagnostics requests.
#[derive(Default)]
pub(crate) struct HostLoggingConfigurationReader {
    pending: Option<PendingPhase>,
    next_attempt: Option<Instant>,
    last_revision: Option<u64>,
}

impl HostLoggingConfigurationReader {
    pub(crate) fn tick(
        &mut self,
        now: Instant,
        mut send: impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Option<Value> {
        let Some(phase) = self.pending.take() else {
            return self.submit_if_due(now, &mut send);
        };

        match phase {
            PendingPhase::Waiting { request_id } => match send(Command::TakeResult {
                request: request_id,
            }) {
                Ok(Response::Pending) => {
                    self.pending = Some(PendingPhase::Waiting { request_id });
                    None
                }
                Ok(Response::Result { payload }) => self.release_and_consume(
                    PendingPhase::Releasing {
                        request_id,
                        outcome: ChildOutcome::Payload(payload),
                    },
                    now,
                    &mut send,
                ),
                Ok(Response::Error { code, .. }) if code != "not_found" => self
                    .release_and_consume(
                        PendingPhase::Releasing {
                            request_id,
                            outcome: ChildOutcome::Failure,
                        },
                        now,
                        &mut send,
                    ),
                Ok(Response::Error { .. }) => {
                    self.schedule_retry(now);
                    None
                }
                Ok(_) => self.release_and_consume(
                    PendingPhase::Releasing {
                        request_id,
                        outcome: ChildOutcome::Failure,
                    },
                    now,
                    &mut send,
                ),
                Err(_) => {
                    self.pending = Some(PendingPhase::Waiting { request_id });
                    None
                }
            },
            releasing @ PendingPhase::Releasing { .. } => {
                self.release_and_consume(releasing, now, &mut send)
            }
        }
    }

    fn submit_if_due(
        &mut self,
        now: Instant,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Option<Value> {
        if self
            .next_attempt
            .is_some_and(|next_attempt| now < next_attempt)
        {
            return None;
        }

        match send(Command::Request {
            service: HOST_CONFIGURATION_SERVICE.into(),
            payload: json!({"action":"get"}),
            timeout_ms: REQUEST_TIMEOUT_MS,
            operation_id: HOST_LOGGING_OPERATION_ID.into(),
            parent: None,
        }) {
            Ok(Response::Accepted { id }) => {
                self.pending = Some(PendingPhase::Waiting { request_id: id });
                None
            }
            Ok(_) => {
                self.schedule_retry(now);
                None
            }
            Err(_) => {
                // The request may have been accepted before the transport failed.
                // Reuse the stable operation ID so the broker returns that same request.
                self.schedule_retry(now);
                None
            }
        }
    }

    fn release_and_consume(
        &mut self,
        phase: PendingPhase,
        now: Instant,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Option<Value> {
        let request_id = match &phase {
            PendingPhase::Releasing { request_id, .. } => *request_id,
            PendingPhase::Waiting { .. } => unreachable!("only terminal results are released"),
        };

        match send(Command::ReleaseResult {
            request: request_id,
        }) {
            Ok(Response::Ok) => self.consume_released(phase, now),
            Ok(Response::Error { code, .. }) if code == "not_found" => {
                self.consume_released(phase, now)
            }
            Ok(_) | Err(_) => {
                self.pending = Some(phase);
                None
            }
        }
    }

    fn consume_released(&mut self, phase: PendingPhase, now: Instant) -> Option<Value> {
        let PendingPhase::Releasing { outcome, .. } = phase else {
            unreachable!("only terminal results are consumed")
        };
        let ChildOutcome::Payload(payload) = outcome else {
            self.schedule_retry(now);
            return None;
        };

        let Some(revision) = ready_revision(&payload) else {
            self.schedule_retry(now);
            return None;
        };

        self.next_attempt = Some(now + REFRESH_INTERVAL);
        let is_new_revision = self.last_revision != Some(revision);
        self.last_revision = Some(revision);
        (revision > 0 && is_new_revision).then_some(payload)
    }

    fn schedule_retry(&mut self, now: Instant) {
        self.pending = None;
        self.next_attempt = Some(now + RETRY_DELAY);
    }
}

fn ready_revision(payload: &Value) -> Option<u64> {
    if payload.get("ok").and_then(Value::as_bool) != Some(true)
        || payload.get("ready").and_then(Value::as_bool) != Some(true)
        || payload.get("version").and_then(Value::as_u64) != Some(1)
    {
        return None;
    }

    let revision = payload.get("revision").and_then(Value::as_u64)?;
    let values = payload.get("values").and_then(Value::as_object)?;
    values.get("log_path").and_then(Value::as_str)?;

    let effective = payload.get("effective").and_then(Value::as_object)?;
    let minimum_level = effective.get("min_log_level").and_then(Value::as_str)?;
    if !matches!(
        minimum_level,
        "verbose" | "debug" | "info" | "warning" | "error" | "fatal" | "none"
    ) {
        return None;
    }
    Some(revision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ready_configuration(revision: u64) -> Value {
        json!({
            "ok":true,
            "ready":true,
            "version":1,
            "revision":revision,
            "values":{"log_path":"logs/amaterasu.log"},
            "effective":{
                "log_path":"C:/state/logs/amaterasu.log",
                "min_log_level":"warning",
            },
        })
    }

    #[test]
    fn asynchronously_reads_releases_and_refreshes_persisted_settings() {
        let started = Instant::now();
        let mut reader = HostLoggingConfigurationReader::default();
        let mut lifecycle = Vec::new();

        assert!(
            reader
                .tick(started, |command| {
                    match command {
                        Command::Request {
                            service,
                            payload,
                            operation_id,
                            parent,
                            ..
                        } => {
                            assert_eq!(service, HOST_CONFIGURATION_SERVICE);
                            assert_eq!(payload, json!({"action":"get"}));
                            assert_eq!(operation_id, HOST_LOGGING_OPERATION_ID);
                            assert_eq!(parent, None);
                            lifecycle.push("request");
                            Ok(Response::Accepted { id: 10 })
                        }
                        unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                    }
                })
                .is_none()
        );

        assert!(
            reader
                .tick(started + Duration::from_secs(1), |command| {
                    match command {
                        Command::TakeResult { request: 10 } => {
                            lifecycle.push("take_pending");
                            Ok(Response::Pending)
                        }
                        unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                    }
                })
                .is_none()
        );

        let loaded = reader.tick(started + Duration::from_secs(2), |command| match command {
            Command::TakeResult { request: 10 } => {
                lifecycle.push("take_result");
                Ok(Response::Result {
                    payload: ready_configuration(1),
                })
            }
            Command::ReleaseResult { request: 10 } => {
                lifecycle.push("release");
                Ok(Response::Ok)
            }
            unexpected => panic!("unexpected runtime command: {unexpected:?}"),
        });
        assert_eq!(loaded, Some(ready_configuration(1)));
        assert_eq!(
            lifecycle,
            ["request", "take_pending", "take_result", "release"]
        );

        let refreshed = reader.tick(started + Duration::from_secs(32), |command| match command {
            Command::Request { .. } => Ok(Response::Accepted { id: 11 }),
            unexpected => panic!("unexpected runtime command: {unexpected:?}"),
        });
        assert!(refreshed.is_none());

        let changed = reader.tick(started + Duration::from_secs(33), |command| match command {
            Command::TakeResult { request: 11 } => Ok(Response::Result {
                payload: ready_configuration(2),
            }),
            Command::ReleaseResult { request: 11 } => Ok(Response::Ok),
            unexpected => panic!("unexpected runtime command: {unexpected:?}"),
        });
        assert_eq!(changed, Some(ready_configuration(2)));
    }

    #[test]
    fn cancelled_results_are_released_and_unready_documents_are_retried() {
        let started = Instant::now();
        let mut reader = HostLoggingConfigurationReader::default();
        let mut released = false;

        assert!(
            reader
                .tick(started, |command| match command {
                    Command::Request { .. } => Ok(Response::Accepted { id: 20 }),
                    unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                })
                .is_none()
        );
        assert!(
            reader
                .tick(started + Duration::from_secs(1), |command| match command {
                    Command::TakeResult { request: 20 } => {
                        Ok(Response::error(
                            "cancelled",
                            "configuration request cancelled",
                        ))
                    }
                    Command::ReleaseResult { request: 20 } => {
                        Err("temporary release failure".into())
                    }
                    unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                })
                .is_none()
        );
        assert!(
            reader
                .tick(started + Duration::from_secs(2), |command| match command {
                    Command::ReleaseResult { request: 20 } => {
                        released = true;
                        Ok(Response::Ok)
                    }
                    unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                })
                .is_none()
        );
        assert!(released);
        assert!(
            reader
                .tick(started + Duration::from_secs(3), |_| {
                    panic!("retry delay should prevent an early configuration request")
                })
                .is_none()
        );
        assert!(
            reader
                .tick(started + Duration::from_secs(7), |command| match command {
                    Command::Request { .. } => Ok(Response::Accepted { id: 21 }),
                    unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                })
                .is_none()
        );

        assert!(
            reader
                .tick(started + Duration::from_secs(8), |command| match command {
                    Command::TakeResult { request: 21 } => Ok(Response::Result {
                        payload: json!({
                            "ok":true,
                            "ready":false,
                            "version":1,
                            "revision":1,
                        }),
                    }),
                    Command::ReleaseResult { request: 21 } => Ok(Response::Ok),
                    unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                })
                .is_none()
        );
    }

    #[test]
    fn revision_zero_keeps_the_trusted_bootstrap_policy() {
        let started = Instant::now();
        let mut reader = HostLoggingConfigurationReader::default();

        assert!(
            reader
                .tick(started, |command| match command {
                    Command::Request { .. } => Ok(Response::Accepted { id: 30 }),
                    unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                })
                .is_none()
        );
        assert!(
            reader
                .tick(started + Duration::from_secs(1), |command| match command {
                    Command::TakeResult { request: 30 } => Ok(Response::Result {
                        payload: ready_configuration(0),
                    }),
                    Command::ReleaseResult { request: 30 } => Ok(Response::Ok),
                    unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                })
                .is_none()
        );
        assert!(
            reader
                .tick(started + Duration::from_secs(31), |command| match command {
                    Command::Request { .. } => Ok(Response::Accepted { id: 31 }),
                    unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                })
                .is_none()
        );
    }

    #[test]
    fn a_lost_submit_acknowledgement_retries_with_the_same_operation_id() {
        let started = Instant::now();
        let mut reader = HostLoggingConfigurationReader::default();
        let mut first_operation_id = None;

        assert!(
            reader
                .tick(started, |command| match command {
                    Command::Request { operation_id, .. } => {
                        assert_eq!(operation_id, HOST_LOGGING_OPERATION_ID);
                        first_operation_id = Some(operation_id);
                        Err("request acknowledgement was lost".into())
                    }
                    unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                })
                .is_none()
        );

        assert!(
            reader
                .tick(started + Duration::from_secs(5), |command| match command {
                    Command::Request { operation_id, .. } => {
                        assert_eq!(operation_id, HOST_LOGGING_OPERATION_ID);
                        assert_eq!(Some(operation_id), first_operation_id);
                        Ok(Response::Accepted { id: 40 })
                    }
                    unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                })
                .is_none()
        );
    }

    // Restart simulations are sequential; simultaneous readers would share this caller-scoped key.
    #[test]
    fn a_restarted_single_reader_reattaches_to_its_pending_request() {
        let started = Instant::now();
        let mut original_reader = HostLoggingConfigurationReader::default();
        let mut retained_request_id = None;
        let mut enqueued_requests = 0;

        assert!(
            original_reader
                .tick(started, |command| match command {
                    Command::Request { operation_id, .. } => {
                        assert_eq!(operation_id, HOST_LOGGING_OPERATION_ID);
                        retained_request_id = Some(50);
                        enqueued_requests += 1;
                        Ok(Response::Accepted { id: 50 })
                    }
                    unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                })
                .is_none()
        );
        drop(original_reader);

        let mut restarted_reader = HostLoggingConfigurationReader::default();
        assert!(
            restarted_reader
                .tick(started + Duration::from_secs(1), |command| match command {
                    Command::Request { operation_id, .. } => {
                        assert_eq!(operation_id, HOST_LOGGING_OPERATION_ID);
                        Ok(Response::Accepted {
                            id: retained_request_id.expect("the original request is retained"),
                        })
                    }
                    unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                })
                .is_none()
        );
        assert!(
            restarted_reader
                .tick(started + Duration::from_secs(2), |command| match command {
                    Command::TakeResult { request: 50 } => Ok(Response::Pending),
                    unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                })
                .is_none()
        );

        assert_eq!(enqueued_requests, 1);
        assert_eq!(retained_request_id, Some(50));
    }

    #[test]
    fn a_restarted_single_reader_reuses_a_completion_until_release_then_refreshes() {
        let started = Instant::now();
        let mut original_reader = HostLoggingConfigurationReader::default();
        let mut retained_request_id = None;
        let mut next_request_id = 70;
        let mut enqueued_requests = 0;
        let mut released_requests = Vec::new();

        assert!(
            original_reader
                .tick(started, |command| match command {
                    Command::Request { operation_id, .. } => {
                        assert_eq!(operation_id, HOST_LOGGING_OPERATION_ID);
                        retained_request_id = Some(next_request_id);
                        enqueued_requests += 1;
                        Ok(Response::Accepted {
                            id: next_request_id,
                        })
                    }
                    unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                })
                .is_none()
        );
        drop(original_reader);

        let mut restarted_reader = HostLoggingConfigurationReader::default();
        assert!(
            restarted_reader
                .tick(started + Duration::from_secs(1), |command| match command {
                    Command::Request { operation_id, .. } => {
                        assert_eq!(operation_id, HOST_LOGGING_OPERATION_ID);
                        Ok(Response::Accepted {
                            id: retained_request_id.expect("the completed request is retained"),
                        })
                    }
                    unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                })
                .is_none()
        );

        let first_configuration = ready_configuration(1);
        let loaded =
            restarted_reader.tick(started + Duration::from_secs(2), |command| match command {
                Command::TakeResult { request } => {
                    assert_eq!(Some(request), retained_request_id);
                    Ok(Response::Result {
                        payload: first_configuration.clone(),
                    })
                }
                Command::ReleaseResult { request } => {
                    assert_eq!(Some(request), retained_request_id);
                    released_requests.push(request);
                    retained_request_id = None;
                    Ok(Response::Ok)
                }
                unexpected => panic!("unexpected runtime command: {unexpected:?}"),
            });
        assert_eq!(loaded, Some(first_configuration));

        next_request_id += 1;
        assert!(
            restarted_reader
                .tick(started + Duration::from_secs(32), |command| match command {
                    Command::Request { operation_id, .. } => {
                        assert_eq!(operation_id, HOST_LOGGING_OPERATION_ID);
                        assert!(retained_request_id.is_none());
                        retained_request_id = Some(next_request_id);
                        enqueued_requests += 1;
                        Ok(Response::Accepted {
                            id: next_request_id,
                        })
                    }
                    unexpected => panic!("unexpected runtime command: {unexpected:?}"),
                })
                .is_none()
        );

        let refreshed_configuration = ready_configuration(2);
        let refreshed =
            restarted_reader.tick(started + Duration::from_secs(33), |command| match command {
                Command::TakeResult { request } => {
                    assert_eq!(Some(request), retained_request_id);
                    Ok(Response::Result {
                        payload: refreshed_configuration.clone(),
                    })
                }
                Command::ReleaseResult { request } => {
                    assert_eq!(Some(request), retained_request_id);
                    released_requests.push(request);
                    retained_request_id = None;
                    Ok(Response::Ok)
                }
                unexpected => panic!("unexpected runtime command: {unexpected:?}"),
            });

        assert_eq!(refreshed, Some(refreshed_configuration));
        assert_eq!(enqueued_requests, 2);
        assert_eq!(released_requests, [70, 71]);
        assert_eq!(retained_request_id, None);
    }
}
