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
use super::lifecycle_issues::plain_code;
use amaterasu_module_sdk::LogLevel;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

const LIFECYCLE_SERVICE: &str = "amaterasu.session-lifecycle/1";
const CALL_TIMEOUT_MS: u64 = 5_000;
const CALL_DEADLINE: Duration = Duration::from_secs(5);
const UNAVAILABLE_RETRY_INTERVAL: Duration = Duration::from_secs(5);
const MAX_SNAPSHOT_ATTEMPTS: u8 = 6;
const MAX_CANDIDATES: usize = 128;
/// A truncated listing is resumed only after progress, and at most this often,
/// so sessions the coordinator keeps refusing cannot loop the sweep.
const MAX_LISTING_ROUNDS: u8 = 8;
const MAX_LOGGED_REASONS: usize = 8;

pub(super) type SweepEvent = (LogLevel, String);

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
        round: Abandoning,
        active: Option<AbandonmentCall>,
    },
    Idle,
}

/// One pass over a stranded-session listing.
struct Abandoning {
    current_epoch: String,
    remaining: VecDeque<StrandedSession>,
    listing_truncated: bool,
    accepted: usize,
    failures: BTreeMap<(&'static str, String), usize>,
    other_failures: usize,
}

impl Abandoning {
    fn record_failure(&mut self, source: &'static str, reason: &str) {
        let key = (source, reason.to_owned());
        if let Some(count) = self.failures.get_mut(&key) {
            *count += 1;
        } else if self.failures.len() < MAX_LOGGED_REASONS {
            self.failures.insert(key, 1);
        } else {
            self.other_failures += 1;
        }
    }
}

struct AbandonmentCall {
    active: ActiveCall,
    operation_id: String,
    generation: u64,
}

pub(super) struct RestartSweep {
    step: Step,
    snapshot_operation_prefix: String,
    snapshot_sequence: u64,
    listing_rounds: u8,
}

impl RestartSweep {
    pub(super) fn disabled(snapshot_operation_prefix: String) -> Self {
        Self {
            step: Step::Disabled,
            snapshot_operation_prefix,
            snapshot_sequence: 0,
            listing_rounds: 0,
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
            self.listing_rounds = 0;
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
            Step::Abandoning { round, active } => self.poll_abandoning(round, active, now, send),
        }
    }

    pub(super) fn shutdown(&mut self, send: &mut Send<'_>) {
        let active = match &mut self.step {
            Step::Snapshot { active, .. } => Some(active),
            Step::Abandoning { active, .. } => active.as_mut().map(|attempt| &mut attempt.active),
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
                // The full snapshot is bounded and refuses retained history
                // past the broker limit; this listing stays small.
                json!({"operation":"snapshot","restart_stranded":true}),
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
                    let reason = if payload["status"] == "ok" {
                        "invalid_snapshot"
                    } else {
                        provider_reason(&payload)
                    };
                    return Some((
                        LogLevel::Warning,
                        format!(
                            "The lifecycle coordinator could not list sessions after restart: {reason}."
                        ),
                    ));
                };
                if stranded.is_empty() {
                    return None;
                }
                self.listing_rounds = self.listing_rounds.saturating_add(1);
                self.step = Step::Abandoning {
                    round: Abandoning {
                        current_epoch,
                        remaining: stranded.into(),
                        listing_truncated: payload["truncated"] == true,
                        accepted: 0,
                        failures: BTreeMap::new(),
                        other_failures: 0,
                    },
                    active: None,
                };
                None
            }
            Outcome::Rejected | Outcome::Settled if attempts < MAX_SNAPSHOT_ATTEMPTS => {
                // Session lifecycle may not have registered its service yet.
                self.step = Step::AwaitingSnapshot {
                    not_before: Some(now + UNAVAILABLE_RETRY_INTERVAL),
                    attempts,
                };
                None
            }
            outcome @ (Outcome::Rejected | Outcome::Settled | Outcome::Uncertain) => Some((
                LogLevel::Warning,
                format!(
                    "Could not query sessions after {attempts} attempts. The broker reported {}.",
                    broker_reason(&outcome, &active.call),
                ),
            )),
        }
    }

    fn poll_abandoning(
        &mut self,
        mut round: Abandoning,
        active: Option<AbandonmentCall>,
        now: Instant,
        send: &mut Send<'_>,
    ) -> Option<SweepEvent> {
        let mut active = match active {
            Some(active) => active,
            None => {
                let Some(session) = round.remaining.pop_front() else {
                    return Some(self.finish_round(&round));
                };
                abandon_call(&round.current_epoch, &session, now)
            }
        };
        let outcome = poll_call(&mut active.active, now, send);
        let (source, reason) = match &outcome {
            Outcome::Waiting => {
                self.step = Step::Abandoning {
                    round,
                    active: Some(active),
                };
                return None;
            }
            Outcome::Result(payload) => {
                active.active.call.release_result(send);
                if abandonment_accepted(payload) {
                    round.accepted += 1;
                    self.step = Step::Abandoning {
                        round,
                        active: None,
                    };
                    return None;
                }
                ("coordinator", provider_reason(payload))
            }
            outcome => ("broker", broker_reason(outcome, &active.active.call)),
        };
        round.record_failure(source, reason);
        self.step = Step::Abandoning {
            round,
            active: None,
        };
        Some((
            LogLevel::Debug,
            format!(
                "Recovery of {} (generation {}) failed. The {source} reported {reason}.",
                active.operation_id, active.generation,
            ),
        ))
    }

    /// Ask for the rest of a truncated listing once this round made progress.
    fn finish_round(&mut self, round: &Abandoning) -> SweepEvent {
        let will_continue = round.listing_truncated
            && round.accepted > 0
            && self.listing_rounds < MAX_LISTING_ROUNDS;
        if will_continue {
            self.step = awaiting_snapshot();
        }
        let failed = round.failures.values().sum::<usize>() + round.other_failures;
        let mut reasons = round
            .failures
            .iter()
            .map(|((source, reason), count)| format!("{source}: {reason} ({count})"))
            .collect::<Vec<_>>();
        if round.other_failures > 0 {
            reasons.push(format!("other causes ({})", round.other_failures));
        }
        let level = if failed > 0 || (round.listing_truncated && !will_continue) {
            LogLevel::Warning
        } else {
            LogLevel::Debug
        };
        let mut summary = format!(
            "Restart recovery, round {}: requests accepted: {}. Failed: {failed}.",
            self.listing_rounds, round.accepted
        );
        if !reasons.is_empty() {
            summary.push_str(&format!(" Reasons: {}.", reasons.join(", ")));
        }
        if round.listing_truncated {
            summary.push_str(if will_continue {
                " Reading the next group of sessions."
            } else {
                " The session list was incomplete."
            });
        }
        (level, summary)
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
fn abandon_call(current_epoch: &str, session: &StrandedSession, now: Instant) -> AbandonmentCall {
    let operation_id = abandonment_operation_id(current_epoch, session);
    let payload = json!({"operation":"recover_session","restart_abandonment":true,"operation_id":operation_id,
        "target":{"kind":"session","host_epoch":current_epoch,
            "session_id":session.session_id,"generation":session.generation}});
    AbandonmentCall {
        active: ActiveCall {
            call: Call::new(
                LIFECYCLE_SERVICE,
                payload,
                operation_id.clone(),
                CALL_TIMEOUT_MS,
            ),
            deadline: now + CALL_DEADLINE,
        },
        operation_id,
        generation: session.generation,
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

fn abandonment_accepted(payload: &Value) -> bool {
    matches!(payload["status"].as_str(), Some("pending" | "succeeded"))
}

fn provider_reason(payload: &Value) -> &str {
    payload["reason"]
        .as_str()
        .filter(|code| plain_code(code))
        .unwrap_or("no_reason_code")
}

fn broker_reason<'a>(outcome: &Outcome, call: &'a Call) -> &'a str {
    match outcome {
        Outcome::Rejected => call
            .rejection_code()
            .filter(|code| plain_code(code))
            .unwrap_or("request_rejected"),
        Outcome::Settled => "request_settled_without_result",
        _ => "request_settlement_unconfirmed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::{Command, Response};

    #[test]
    fn recovery_logs_one_warning_with_bounded_provider_and_broker_reasons() {
        let mut sweep = RestartSweep::disabled("test".into());
        sweep.enable();
        let sessions: Vec<_> = (1..=5)
            .map(|generation| {
                json!({
                    "session_id": format!("private-session-{generation}"),
                    "host_epoch":"previous","generation":generation,"phase":"recovery_required"
                })
            })
            .collect();
        let now = Instant::now();
        let events: Vec<_> = (0..32)
            .filter_map(|_| sweep.poll(now, &mut |command| {
                let reply = match command {
                    Command::Request { payload, .. } if payload["operation"] == "snapshot" =>
                        Response::Accepted { id: 10 },
                    Command::Request { payload, .. } => {
                        let id = payload["target"]["generation"].as_u64().unwrap();
                        if id == 4 {
                            Response::error("permission_denied", "private broker diagnostic")
                        } else {
                            Response::Accepted { id }
                        }
                    }
                    Command::TakeResult { request } => Response::Result {
                        payload: match request {
                            10 => json!({"status":"ok","host_epoch":"current","sessions":sessions}),
                            1 | 2 => json!({"status":"rejected","reason":"release_work_unavailable"}),
                            3 => json!({"status":"rejected","reason":"private failure payload\n"}),
                            5 => json!({"status":"pending"}),
                            _ => panic!("unexpected request {request}"),
                        },
                    },
                    Command::ReleaseResult { .. } => Response::Ok,
                    unexpected => panic!("unexpected command: {unexpected:?}"),
                };
                Ok(reply)
            }))
            .collect();
        assert_eq!(events.len(), 5);
        assert!(
            events[..4]
                .iter()
                .all(|(level, message)| *level == LogLevel::Debug
                    && message.contains("Recovery of nvhttp-restart-abandon-"))
        );
        let (level, summary) = &events[4];
        assert_eq!(*level, LogLevel::Warning);
        assert!(summary.contains("requests accepted: 1. Failed: 4."));
        assert!(summary.contains("broker: permission_denied (1)"));
        assert!(summary.contains("coordinator: no_reason_code (1)"));
        assert!(summary.contains("coordinator: release_work_unavailable (2)"));
        assert!(
            events
                .iter()
                .all(|(_, message)| !message.contains("private"))
        );
    }

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
