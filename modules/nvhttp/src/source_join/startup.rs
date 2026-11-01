//! Fence the old protocol incarnation and wait for a new pairing epoch.
use amaterasu_module_sdk::{
    Command, ModuleResult, Response,
    effect_source_retirement::{ProtocolFenceReceipt, Request},
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const PAIRING: &str = "amaterasu.pairing-effects/1";

enum Phase {
    StatusSubmit,
    StatusWait(u64),
    StatusRelease(u64, Value),
    FenceSubmit(String),
    FenceWait(String, u64),
    FenceRelease(String, u64, Value),
    FenceRetryRelease(String, u64),
}

pub struct Startup {
    phase: Phase,
    fenced_epoch: Option<String>,
    next_attempt: Instant,
}

impl Startup {
    pub fn new() -> Self {
        Self {
            phase: Phase::StatusSubmit,
            fenced_epoch: None,
            next_attempt: Instant::now(),
        }
    }

    /// The first fence makes old binds fail; only a later epoch, after the
    /// selected effect owner has retired workers and drained, reopens joins.
    pub fn advance(&mut self, mut send: impl FnMut(Command) -> ModuleResult<Response>) -> bool {
        match &mut self.phase {
            Phase::StatusSubmit => {
                if Instant::now() < self.next_attempt {
                    return false;
                }
                match send(Command::Request {
                    service: PAIRING.into(),
                    payload: json!({"action":"status"}),
                    timeout_ms: 5000,
                    operation_id: "nvhttp.source.startup-status".into(),
                    parent: None,
                }) {
                    Ok(Response::Accepted { id }) => self.phase = Phase::StatusWait(id),
                    _ => self.next_attempt = Instant::now() + Duration::from_secs(1),
                }
            }
            Phase::StatusWait(id) => match send(Command::TakeResult { request: *id }) {
                Ok(Response::Pending) => {}
                Ok(Response::Result { payload }) => self.phase = Phase::StatusRelease(*id, payload),
                Ok(Response::Error { .. }) => self.phase = Phase::StatusRelease(*id, json!({})),
                _ => {}
            },
            Phase::StatusRelease(id, payload) => {
                if released(send(Command::ReleaseResult { request: *id })) {
                    if let Some(epoch) = status_epoch(payload) {
                        if let Some(previous) = &self.fenced_epoch {
                            if epoch != previous && clean_status(payload) {
                                return true;
                            }
                        } else {
                            self.phase = Phase::FenceSubmit(epoch.to_owned());
                            return false;
                        }
                    }
                    self.phase = Phase::StatusSubmit;
                    self.next_attempt = Instant::now() + Duration::from_secs(1);
                }
            }
            Phase::FenceSubmit(epoch) => {
                let operation_id = format!("nvhttp-restart-{epoch}");
                let payload = serde_json::to_value(Request::FenceProtocolRestart {
                    epoch: epoch.clone(),
                    operation_id: operation_id.clone(),
                })
                .expect("fence request is JSON");
                match send(Command::Request {
                    service: PAIRING.into(),
                    payload,
                    timeout_ms: 5000,
                    operation_id: format!("nvhttp.{operation_id}"),
                    parent: None,
                }) {
                    Ok(Response::Accepted { id }) => {
                        self.phase = Phase::FenceWait(epoch.clone(), id)
                    }
                    Ok(Response::Error { .. }) => {
                        self.phase = Phase::StatusSubmit;
                        self.next_attempt = Instant::now() + Duration::from_secs(1);
                    }
                    _ => {} // Retry the identical operation if admission reply was lost.
                }
            }
            Phase::FenceWait(epoch, id) => match send(Command::TakeResult { request: *id }) {
                Ok(Response::Pending) => {}
                Ok(Response::Result { payload }) => {
                    self.phase = Phase::FenceRelease(epoch.clone(), *id, payload)
                }
                Ok(Response::Error { .. }) => {
                    self.phase = Phase::FenceRetryRelease(epoch.clone(), *id)
                }
                _ => {}
            },
            Phase::FenceRetryRelease(epoch, id) => {
                if released(send(Command::ReleaseResult { request: *id })) {
                    self.phase = Phase::FenceSubmit(epoch.clone());
                }
            }
            Phase::FenceRelease(epoch, id, payload) => {
                if released(send(Command::ReleaseResult { request: *id })) {
                    let operation_id = format!("nvhttp-restart-{epoch}");
                    let valid = serde_json::from_value::<ProtocolFenceReceipt>(payload.clone())
                        .ok()
                        .is_some_and(|receipt| receipt.matches(epoch, &operation_id));
                    self.fenced_epoch = valid.then(|| epoch.clone());
                    self.phase = Phase::StatusSubmit;
                    self.next_attempt = Instant::now() + Duration::from_secs(1);
                }
            }
        }
        false
    }
}

fn released(result: ModuleResult<Response>) -> bool {
    match result {
        Ok(Response::Ok) => true,
        Ok(Response::Error { code, .. }) if code == "not_found" => true,
        _ => false,
    }
}

fn status_epoch(value: &Value) -> Option<&str> {
    if value["ok"] != true {
        return None;
    }
    value["epoch"].as_str().filter(|epoch| {
        epoch.len() == 32
            && epoch
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

pub(super) fn clean_status(value: &Value) -> bool {
    value["ok"] == true
        && status_epoch(value).is_some()
        && value["owner_epoch"].as_str().is_some_and(|owner| {
            !owner.is_empty() && owner.len() <= 128 && !owner.chars().any(char::is_control)
        })
        && value["startup_recovery_required"] == false
        && value["fenced"] == false
        && value["drain_required"] == false
        && value["in_flight"].as_u64() == Some(0)
        && value["bound_sources"].as_u64() == Some(0)
        && value["pending_source_retirements"].as_u64() == Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_requires_current_unfenced_empty_authority_and_live_owner() {
        let ready = json!({"ok":true,"epoch":"a".repeat(32),"owner_epoch":"owner",
            "startup_recovery_required":false,"fenced":false,"drain_required":false,
            "in_flight":0,"bound_sources":0,"pending_source_retirements":0});
        assert!(clean_status(&ready));
        for (field, value) in [
            ("startup_recovery_required", json!(true)),
            ("fenced", json!(true)),
            ("drain_required", json!(true)),
            ("in_flight", json!(1)),
            ("bound_sources", json!(1)),
            ("pending_source_retirements", json!(1)),
            ("owner_epoch", Value::Null),
        ] {
            let mut changed = ready.clone();
            changed[field] = value;
            assert!(!clean_status(&changed), "{field}");
        }
        let mut legacy = ready;
        legacy.as_object_mut().unwrap().remove("bound_sources");
        assert!(!clean_status(&legacy));
    }

    #[test]
    fn startup_fences_the_old_epoch_before_accepting_a_new_drained_one() {
        let old = "a".repeat(32);
        let new = "b".repeat(32);
        let mut startup = Startup::new();
        let status = std::cell::RefCell::new(json!({"ok":true,"epoch":old,"owner_epoch":"owner",
            "startup_recovery_required":false,"fenced":false,"drain_required":false,
            "in_flight":0,"bound_sources":0,"pending_source_retirements":0}));
        let queried_fence = std::cell::Cell::new(false);
        let mut send = |command| {
            Ok(match command {
                Command::Request { payload, .. } if payload["action"] == "status" => {
                    Response::Accepted { id: 1 }
                }
                Command::Request { payload, .. }
                    if payload["action"] == "fence_protocol_restart" =>
                {
                    queried_fence.set(true);
                    assert_eq!(payload["epoch"], old);
                    Response::Accepted { id: 2 }
                }
                Command::TakeResult { request: 1 } => Response::Result {
                    payload: status.borrow().clone(),
                },
                Command::TakeResult { request: 2 } => Response::Result {
                    payload: json!({"ok":true,"epoch":old,
                "operation_id":format!("nvhttp-restart-{old}"),"state":"fenced","fence_generation":1}),
                },
                Command::ReleaseResult { .. } => Response::Ok,
                other => panic!("unexpected {other:?}"),
            })
        };
        for _ in 0..6 {
            assert!(!startup.advance(&mut send));
        }
        assert!(queried_fence.get());
        startup.next_attempt = Instant::now();
        for _ in 0..3 {
            assert!(!startup.advance(&mut send));
        }
        status.borrow_mut()["epoch"] = json!(new);
        startup.next_attempt = Instant::now();
        assert!(!startup.advance(&mut send));
        assert!(!startup.advance(&mut send));
        assert!(startup.advance(&mut send));
    }
}
