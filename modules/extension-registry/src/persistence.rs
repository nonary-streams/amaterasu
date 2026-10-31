//! Bounded, parent-scoped requests to the durable state service.
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::Instant;

const STATE_SERVICE: &str = "amaterasu.state/1";
const MAX_QUEUED: usize = 32;
// Leave room for the state response and runtime envelope.
const MAX_ENTRY_BYTES: usize = 12_000;

struct Request {
    parent: u64,
    deadline: Instant,
    payload: Value,
    operation_id: String,
}

struct Pending {
    request: Request,
    phase: Phase,
    submission_uncertain: bool,
}

enum Phase {
    Submit,
    Waiting(u64),
    Releasing(u64, Value),
}

#[derive(Default)]
pub struct Store {
    queue: VecDeque<Request>,
    pending: Option<Pending>,
    sequence: u64,
}

impl Store {
    pub fn get(&mut self, parent: u64, key: String, deadline: Instant) -> Result<(), Value> {
        self.enqueue(parent, deadline, json!({"action":"get", "key":key}))
    }

    pub fn set(
        &mut self,
        parent: u64,
        key: String,
        value: Value,
        expected_state_revision: u64,
        deadline: Instant,
    ) -> Result<(), Value> {
        let entry = json!({"key":key, "value":value});
        if !fits(&entry) {
            return Err(error(
                "resource_exhausted",
                "state entry exceeds size limit",
            ));
        }
        self.enqueue(
            parent,
            deadline,
            json!({
                "action":"set", "key":key, "value":value,
                "expected_revision":expected_state_revision
            }),
        )
    }

    fn enqueue(&mut self, parent: u64, deadline: Instant, payload: Value) -> Result<(), Value> {
        if self.queue.len() + usize::from(self.pending.is_some()) >= MAX_QUEUED {
            return Err(error("resource_exhausted", "state request queue is full"));
        }
        if !fits(&payload) {
            return Err(error(
                "resource_exhausted",
                "state request exceeds size limit",
            ));
        }
        let sequence = self.sequence;
        self.sequence = self.sequence.checked_add(1).ok_or_else(|| {
            error(
                "resource_exhausted",
                "state operation identifiers exhausted",
            )
        })?;
        self.queue.push_back(Request {
            parent,
            deadline,
            payload,
            operation_id: format!("extension-registry.{parent}.{sequence}"),
        });
        Ok(())
    }

    /// Advance at most one child operation. Completed payloads retain their parent ID.
    pub fn tick(
        &mut self,
        mut send: impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<Vec<(u64, Value)>> {
        let Some(mut pending) = self.pending.take().or_else(|| {
            self.queue.pop_front().map(|request| Pending {
                request,
                phase: Phase::Submit,
                submission_uncertain: false,
            })
        }) else {
            return Ok(Vec::new());
        };
        let parent = pending.request.parent;
        if pending.request.deadline <= Instant::now() {
            let outcome = match pending.phase {
                Phase::Submit if pending.submission_uncertain => {
                    uncertain_outcome(&pending.request)
                }
                Phase::Submit => error("deadline_exceeded", "state request deadline elapsed"),
                Phase::Waiting(child) => {
                    let outcome = match send(Command::TakeResult { request: child }) {
                        Ok(Response::Result { payload }) => payload,
                        _ => {
                            // The provider can finish a write even after cancellation.
                            let _ = send(Command::Cancel { request: child });
                            uncertain_outcome(&pending.request)
                        }
                    };
                    let _ = send(Command::ReleaseResult { request: child });
                    outcome
                }
                Phase::Releasing(child, outcome) => {
                    // The result is already known; a lost release acknowledgement must
                    // not turn a durable success into an ambiguous write.
                    let _ = send(Command::ReleaseResult { request: child });
                    outcome
                }
            };
            return Ok(vec![(parent, outcome)]);
        }
        match pending.phase {
            Phase::Submit => {
                let remaining = pending
                    .request
                    .deadline
                    .saturating_duration_since(Instant::now());
                let timeout_ms = remaining.as_millis().min(30_000) as u64;
                if timeout_ms == 0 {
                    return Ok(vec![(
                        parent,
                        if pending.submission_uncertain {
                            uncertain_outcome(&pending.request)
                        } else {
                            error("deadline_exceeded", "state request deadline elapsed")
                        },
                    )]);
                }
                match send(Command::Request {
                    service: STATE_SERVICE.into(),
                    payload: pending.request.payload.clone(),
                    timeout_ms,
                    operation_id: pending.request.operation_id.clone(),
                    parent: Some(parent),
                }) {
                    Ok(Response::Accepted { id }) => pending.phase = Phase::Waiting(id),
                    Ok(Response::Error { code, message }) => {
                        let outcome = if pending.submission_uncertain {
                            uncertain_outcome(&pending.request)
                        } else {
                            error(&code, &message)
                        };
                        return Ok(vec![(parent, outcome)]);
                    }
                    Ok(_) => {
                        return Ok(vec![(
                            parent,
                            error(
                                "invalid_state_response",
                                "unexpected state submission response",
                            ),
                        )]);
                    }
                    // Retry the same operation ID: submission may have succeeded before the transport failed.
                    Err(_) => pending.submission_uncertain = true,
                }
            }
            Phase::Waiting(child) => match send(Command::TakeResult { request: child }) {
                Ok(Response::Pending) | Err(_) => {}
                Ok(Response::Result { payload }) => {
                    pending.phase = Phase::Releasing(child, payload)
                }
                Ok(Response::Error { code, message })
                    if code == "not_found" || code == "permission_denied" =>
                {
                    return Ok(vec![(
                        parent,
                        if code == "not_found" {
                            uncertain_outcome(&pending.request)
                        } else {
                            error(&code, &message)
                        },
                    )]);
                }
                Ok(Response::Error { code, message }) => {
                    let outcome = if code == "deadline_exceeded" || code == "cancelled" {
                        uncertain_outcome(&pending.request)
                    } else {
                        error(&code, &message)
                    };
                    pending.phase = Phase::Releasing(child, outcome)
                }
                Ok(_) => {
                    pending.phase = Phase::Releasing(
                        child,
                        error("invalid_state_response", "unexpected state result response"),
                    )
                }
            },
            Phase::Releasing(child, ref outcome) => {
                match send(Command::ReleaseResult { request: child }) {
                    Ok(Response::Ok) => return Ok(vec![(parent, outcome.clone())]),
                    Ok(Response::Error { code, .. }) if code == "not_found" => {
                        return Ok(vec![(parent, outcome.clone())]);
                    }
                    Ok(_) | Err(_) => {}
                }
            }
        }
        self.pending = Some(pending);
        Ok(Vec::new())
    }
}

fn uncertain_outcome(request: &Request) -> Value {
    if request.payload.get("action").and_then(Value::as_str) == Some("set") {
        error(
            "write_outcome_unknown",
            "state write may have committed; reload state before retrying",
        )
    } else {
        error("deadline_exceeded", "state request did not return a result")
    }
}

fn fits(value: &Value) -> bool {
    serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= MAX_ENTRY_BYTES)
}

fn error(code: &str, message: &str) -> Value {
    json!({"ok":false, "error":{"code":code, "message":message}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn child_result_is_released_before_completion_and_release_retries() {
        let mut store = Store::default();
        store
            .get(42, "key".into(), Instant::now() + Duration::from_secs(5))
            .unwrap();
        assert!(
            store
                .tick(|cmd| match cmd {
                    Command::Request {
                        parent, payload, ..
                    } => {
                        assert_eq!(parent, Some(42));
                        assert_eq!(payload, json!({"action":"get", "key":"key"}));
                        Ok(Response::Accepted { id: 7 })
                    }
                    _ => panic!("expected request"),
                })
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .tick(|cmd| match cmd {
                    Command::TakeResult { request: 7 } => Ok(Response::Result {
                        payload: json!({"ok":true,"revision":3,"found":true,"value":"v"})
                    }),
                    _ => panic!("expected take"),
                })
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .tick(|cmd| match cmd {
                    Command::ReleaseResult { request: 7 } => Err("lost acknowledgement".into()),
                    _ => panic!("expected release"),
                })
                .unwrap()
                .is_empty()
        );
        let result = store
            .tick(|cmd| match cmd {
                Command::ReleaseResult { request: 7 } => {
                    Ok(Response::error("not_found", "already released"))
                }
                _ => panic!("expected release retry"),
            })
            .unwrap();
        assert_eq!(
            result,
            vec![(42, json!({"ok":true,"revision":3,"found":true,"value":"v"}))]
        );
    }

    #[test]
    fn set_preserves_cas_and_expired_requests_are_not_submitted() {
        let mut store = Store::default();
        store
            .set(
                5,
                "key".into(),
                json!({"x":1}),
                9,
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap();
        store
            .tick(|cmd| match cmd {
                Command::Request { payload, .. } => {
                    assert_eq!(payload["expected_revision"], 9);
                    Ok(Response::error("revision_conflict", "changed"))
                }
                _ => panic!("expected request"),
            })
            .unwrap();
        store.get(6, "key".into(), Instant::now()).unwrap();
        let result = store
            .tick(|_| panic!("expired operation submitted"))
            .unwrap();
        assert_eq!(result[0].0, 6);
        assert_eq!(result[0].1["error"]["code"], "deadline_exceeded");
    }

    #[test]
    fn queue_and_encoded_payload_are_bounded() {
        let mut store = Store::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        assert_eq!(
            store
                .set(
                    1,
                    "key".into(),
                    json!("x".repeat(MAX_ENTRY_BYTES)),
                    0,
                    deadline
                )
                .unwrap_err()["error"]["code"],
            "resource_exhausted"
        );
        for id in 0..MAX_QUEUED {
            store.get(id as u64, "key".into(), deadline).unwrap();
        }
        assert_eq!(
            store.get(99, "key".into(), deadline).unwrap_err()["error"]["code"],
            "resource_exhausted"
        );
    }

    #[test]
    fn expired_inflight_write_is_unknown_and_does_not_block_queue() {
        let mut store = Store::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        store.set(1, "key".into(), json!(1), 0, deadline).unwrap();
        store.get(2, "key".into(), deadline).unwrap();
        store.tick(|_| Ok(Response::Accepted { id: 7 })).unwrap();
        store.pending.as_mut().unwrap().request.deadline = Instant::now();
        let mut calls = Vec::new();
        let result = store
            .tick(|cmd| {
                calls.push(cmd.clone());
                match cmd {
                    Command::TakeResult { .. } => Ok(Response::Pending),
                    Command::Cancel { .. } | Command::ReleaseResult { .. } => {
                        Err("transport unavailable".into())
                    }
                    _ => panic!("unexpected command"),
                }
            })
            .unwrap();
        assert_eq!(result[0].1["error"]["code"], "write_outcome_unknown");
        assert!(matches!(
            calls.as_slice(),
            [
                Command::TakeResult { request: 7 },
                Command::Cancel { request: 7 },
                Command::ReleaseResult { request: 7 }
            ]
        ));
        store
            .tick(|cmd| match cmd {
                Command::Request {
                    parent: Some(2), ..
                } => Ok(Response::Accepted { id: 8 }),
                _ => panic!("queued request did not advance"),
            })
            .unwrap();
    }

    #[test]
    fn deadline_preserves_known_result_and_marks_lost_submission_unknown() {
        let mut store = Store::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        store.set(1, "key".into(), json!(1), 0, deadline).unwrap();
        store
            .tick(|_| Err("lost submit acknowledgement".into()))
            .unwrap();
        store.pending.as_mut().unwrap().request.deadline = Instant::now();
        assert_eq!(
            store.tick(|_| panic!("no child id available")).unwrap()[0].1["error"]["code"],
            "write_outcome_unknown"
        );

        store.set(2, "key".into(), json!(1), 0, deadline).unwrap();
        store.tick(|_| Ok(Response::Accepted { id: 8 })).unwrap();
        store
            .tick(|_| {
                Ok(Response::Result {
                    payload: json!({"ok":true,"revision":1}),
                })
            })
            .unwrap();
        store.pending.as_mut().unwrap().request.deadline = Instant::now();
        let result = store
            .tick(|cmd| match cmd {
                Command::ReleaseResult { request: 8 } => Err("lost release acknowledgement".into()),
                _ => panic!("expected release"),
            })
            .unwrap();
        assert_eq!(result, vec![(2, json!({"ok":true,"revision":1}))]);
    }
}
