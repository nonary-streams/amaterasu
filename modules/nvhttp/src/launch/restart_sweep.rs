//! Startup abandonment of lifecycle sessions interrupted by a host restart.
//!
//! Launch records live only in memory, so after a restart this controller can
//! never report an earlier session's application retired. Without help those
//! sessions stay `recovery_required` forever and eventually exhaust lifecycle
//! capacity. The sweep asks the coordinator to abandon them, which records the
//! application as untracked; it never reports `application_retired`. It owns a
//! separate broker call and never delays a launch.
use super::Send;
use super::call::{Call, Outcome};
use amaterasu_module_sdk::LogLevel;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const LIFECYCLE_SERVICE: &str = "amaterasu.session-lifecycle/1";
const CALL_TIMEOUT_MS: u64 = 5_000;
const CALL_DEADLINE: Duration = Duration::from_secs(5);
const UNAVAILABLE_RETRY_INTERVAL: Duration = Duration::from_secs(5);
const MAX_SNAPSHOT_ATTEMPTS: u8 = 6;
const MAX_CANDIDATES: usize = 128;

/// Fixed, identity-free events for the controller's log.
pub(super) type SweepEvent = (LogLevel, &'static str);

/// A session this owner created in an earlier host epoch.
#[derive(Clone, Debug, PartialEq, Eq)]
struct StrandedSession {
    session_id: String,
    generation: u64,
}

struct ActiveCall {
    call: Call,
    deadline: Instant,
}

enum Step {
    Disabled,
    AwaitingSnapshot {
        not_before: Option<Instant>,
        attempts: u8,
    },
    Snapshot {
        active: ActiveCall,
        attempts: u8,
    },
    Abandoning {
        current_epoch: String,
        remaining: VecDeque<StrandedSession>,
        active: Option<ActiveCall>,
    },
    Idle,
}

pub(super) struct RestartSweep {
    step: Step,
    snapshot_operation_prefix: String,
    snapshot_sequence: u64,
}

impl RestartSweep {
    pub(super) fn disabled(snapshot_operation_prefix: String) -> Self {
        Self {
            step: Step::Disabled,
            snapshot_operation_prefix,
            snapshot_sequence: 0,
        }
    }

    /// Arm the startup sweep once session lifecycle is required.
    pub(super) fn enable(&mut self) {
        if matches!(self.step, Step::Disabled) {
            self.step = awaiting_snapshot();
        }
    }

    /// Run one more sweep after lifecycle capacity refused a preparation.
    pub(super) fn request_retry(&mut self) {
        if matches!(self.step, Step::Idle) {
            self.step = awaiting_snapshot();
        }
    }

    pub(super) fn poll(&mut self, now: Instant, send: &mut Send<'_>) -> Option<SweepEvent> {
        match std::mem::replace(&mut self.step, Step::Idle) {
            Step::Disabled => {
                self.step = Step::Disabled;
                None
            }
            Step::Idle => None,
            Step::AwaitingSnapshot {
                not_before,
                attempts,
            } => {
                if not_before.is_some_and(|instant| now < instant) {
                    self.step = Step::AwaitingSnapshot {
                        not_before,
                        attempts,
                    };
                    return None;
                }
                self.step = Step::Snapshot {
                    active: self.snapshot_call(now),
                    attempts: attempts.saturating_add(1),
                };
                None
            }
            Step::Snapshot { active, attempts } => self.poll_snapshot(active, attempts, now, send),
            Step::Abandoning {
                current_epoch,
                remaining,
                active,
            } => self.poll_abandoning(current_epoch, remaining, active, now, send),
        }
    }

    pub(super) fn shutdown(&mut self, send: &mut Send<'_>) {
        let active = match &mut self.step {
            Step::Snapshot { active, .. } => Some(active),
            Step::Abandoning { active, .. } => active.as_mut(),
            _ => None,
        };
        if let Some(active) = active {
            active.call.shutdown(send);
        }
    }

    fn snapshot_call(&mut self, now: Instant) -> ActiveCall {
        self.snapshot_sequence = self.snapshot_sequence.saturating_add(1);
        let operation_id = format!(
            "{}-restart-sweep-{}",
            self.snapshot_operation_prefix, self.snapshot_sequence
        );
        ActiveCall {
            call: Call::new(
                LIFECYCLE_SERVICE,
                json!({"operation":"snapshot"}),
                operation_id,
                CALL_TIMEOUT_MS,
            ),
            deadline: now + CALL_DEADLINE,
        }
    }

    fn poll_snapshot(
        &mut self,
        mut active: ActiveCall,
        attempts: u8,
        now: Instant,
        send: &mut Send<'_>,
    ) -> Option<SweepEvent> {
        match poll_call(&mut active, now, send) {
            Outcome::Waiting => {
                self.step = Step::Snapshot { active, attempts };
                None
            }
            Outcome::Result(payload) => {
                active.call.release_result(send);
                let Some((current_epoch, stranded)) = stranded_sessions(&payload) else {
                    return Some((LogLevel::Warning, "restart_sweep snapshot_invalid"));
                };
                if stranded.is_empty() {
                    return Some((LogLevel::Debug, "restart_sweep none_stranded"));
                }
                self.step = Step::Abandoning {
                    current_epoch,
                    remaining: stranded.into(),
                    active: None,
                };
                Some((LogLevel::Info, "restart_sweep abandoning_stranded_sessions"))
            }
            Outcome::Rejected | Outcome::Settled if attempts < MAX_SNAPSHOT_ATTEMPTS => {
                // Session lifecycle may not have registered its service yet.
                self.step = Step::AwaitingSnapshot {
                    not_before: Some(now + UNAVAILABLE_RETRY_INTERVAL),
                    attempts,
                };
                None
            }
            Outcome::Rejected | Outcome::Settled | Outcome::Uncertain => {
                Some((LogLevel::Warning, "restart_sweep lifecycle_unavailable"))
            }
        }
    }

    fn poll_abandoning(
        &mut self,
        current_epoch: String,
        mut remaining: VecDeque<StrandedSession>,
        active: Option<ActiveCall>,
        now: Instant,
        send: &mut Send<'_>,
    ) -> Option<SweepEvent> {
        let mut active = match active {
            Some(active) => active,
            None => {
                let Some(session) = remaining.pop_front() else {
                    return Some((LogLevel::Info, "restart_sweep complete"));
                };
                abandon_call(&current_epoch, &session, now)
            }
        };
        let (active, event) = match poll_call(&mut active, now, send) {
            Outcome::Waiting => (Some(active), None),
            Outcome::Result(payload) => {
                active.call.release_result(send);
                (None, Some(abandonment_event(&payload)))
            }
            Outcome::Rejected | Outcome::Settled | Outcome::Uncertain => (
                None,
                Some((LogLevel::Warning, "restart_sweep abandonment_unconfirmed")),
            ),
        };
        self.step = Step::Abandoning {
            current_epoch,
            remaining,
            active,
        };
        event
    }
}

fn awaiting_snapshot() -> Step {
    Step::AwaitingSnapshot {
        not_before: None,
        attempts: 0,
    }
}

fn poll_call(active: &mut ActiveCall, now: Instant, send: &mut Send<'_>) -> Outcome {
    if now >= active.deadline {
        active.call.cancel();
    }
    active.call.poll(send)
}

/// Sessions the coordinator lists for this owner that a previous host epoch
/// left in recovery. Current-epoch sessions belong to live launch records.
fn stranded_sessions(payload: &Value) -> Option<(String, Vec<StrandedSession>)> {
    if payload["status"] != "ok" {
        return None;
    }
    let current_epoch = payload["host_epoch"]
        .as_str()
        .filter(|epoch| !epoch.is_empty())?;
    let stranded = payload["sessions"]
        .as_array()?
        .iter()
        .filter(|session| {
            session["phase"] == "recovery_required"
                && session["host_epoch"]
                    .as_str()
                    .is_some_and(|epoch| !epoch.is_empty() && epoch != current_epoch)
        })
        .filter_map(|session| {
            Some(StrandedSession {
                session_id: session["session_id"]
                    .as_str()
                    .filter(|id| !id.is_empty())?
                    .to_owned(),
                generation: session["generation"].as_u64()?,
            })
        })
        .take(MAX_CANDIDATES)
        .collect();
    Some((current_epoch.to_owned(), stranded))
}

/// The operation ID is derived from the exact target, so a retried sweep in
/// the same epoch replays the coordinator's receipt instead of acting twice.
fn abandon_call(current_epoch: &str, session: &StrandedSession, now: Instant) -> ActiveCall {
    let operation_id = abandonment_operation_id(current_epoch, session);
    let payload = json!({"operation":"recover_session","restart_abandonment":true,"operation_id":operation_id,
        "target":{"kind":"session","host_epoch":current_epoch,
            "session_id":session.session_id,"generation":session.generation}});
    ActiveCall {
        call: Call::new(LIFECYCLE_SERVICE, payload, operation_id, CALL_TIMEOUT_MS),
        deadline: now + CALL_DEADLINE,
    }
}

fn abandonment_operation_id(current_epoch: &str, session: &StrandedSession) -> String {
    let mut digest = Sha256::new();
    for part in [
        current_epoch.as_bytes(),
        session.session_id.as_bytes(),
        &session.generation.to_be_bytes(),
    ] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part);
    }
    let digest: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("nvhttp-restart-abandon-{digest}")
}

fn abandonment_event(payload: &Value) -> SweepEvent {
    match payload["status"].as_str() {
        Some("pending" | "succeeded") => (LogLevel::Info, "restart_sweep session_abandoned"),
        _ => (LogLevel::Warning, "restart_sweep abandonment_refused"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_old_epoch_recovery_sessions_are_stranded() {
        let payload = json!({"status":"ok","host_epoch":"current","sessions":[
            {"session_id":"old","host_epoch":"previous","generation":2,"phase":"recovery_required"},
            {"session_id":"live","host_epoch":"current","generation":1,"phase":"recovery_required"},
            {"session_id":"ended","host_epoch":"previous","generation":1,"phase":"ended"},
            {"session_id":"legacy","generation":1,"phase":"recovery_required"}
        ]});
        let (epoch, stranded) = stranded_sessions(&payload).unwrap();
        assert_eq!(epoch, "current");
        assert_eq!(
            stranded,
            [StrandedSession {
                session_id: "old".into(),
                generation: 2
            }]
        );
        assert!(stranded_sessions(&json!({"status":"rejected"})).is_none());
    }

    #[test]
    fn abandonment_operation_ids_are_deterministic_and_epoch_scoped() {
        let session = StrandedSession {
            session_id: "session".into(),
            generation: 1,
        };
        let first = abandonment_operation_id("epoch", &session);
        assert_eq!(first, abandonment_operation_id("epoch", &session));
        assert_ne!(first, abandonment_operation_id("next-epoch", &session));
        assert!(first.len() <= 128);
    }
}
