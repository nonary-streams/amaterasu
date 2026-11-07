use super::{Record, rpc};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const SERVICE: &str = "amaterasu.session-history/1";
const MAX_RESERVATIONS: usize = 128;
const RESERVATION_MS: u64 = 300_000;

enum Phase {
    Beginning(rpc::Pending),
    Reserved,
    Ending(rpc::Pending),
    Ended,
    Unavailable,
}

struct Reservation {
    rtsp_operation_id: String,
    phase: Phase,
    began_at: u64,
    terminal_at: Option<u64>,
    retry_at: u64,
    callers: Vec<u64>,
    release_callers: Vec<u64>,
    release_requested: bool,
    release_requested_at: Option<u64>,
    completed_at: Option<u64>,
}

#[derive(Default)]
pub(super) struct HistoryActivities {
    entries: BTreeMap<String, Reservation>,
}

impl HistoryActivities {
    pub(super) fn contains(&self, activity_id: &str) -> bool {
        self.entries.contains_key(activity_id)
    }

    /// A reservation or an uncertain cleanup may still own a launch/session.
    pub(super) fn has_active_reservations(&self) -> bool {
        self.entries
            .values()
            .any(|entry| !matches!(entry.phase, Phase::Ended | Phase::Unavailable))
    }

    pub(super) fn reserve(
        &mut self,
        request: u64,
        activity_id: String,
        rtsp_operation_id: String,
        now: u64,
        send: &mut rpc::Send<'_>,
    ) {
        self.prune(now);
        if let Some(entry) = self.entries.get_mut(&activity_id) {
            if entry.rtsp_operation_id != rtsp_operation_id {
                rpc::complete(send, request, super::error("operation_conflict"));
                return;
            }
            match entry.phase {
                Phase::Beginning(_) if entry.callers.len() < 8 => entry.callers.push(request),
                Phase::Beginning(_) => {
                    rpc::complete(send, request, super::error("capacity_exceeded"));
                }
                Phase::Reserved => {
                    rpc::complete(
                        send,
                        request,
                        reply(
                            "reserved",
                            now.saturating_sub(entry.began_at) < RESERVATION_MS,
                            &activity_id,
                            &rtsp_operation_id,
                        ),
                    );
                }
                Phase::Unavailable => {
                    rpc::complete(
                        send,
                        request,
                        reply("reserved", false, &activity_id, &rtsp_operation_id),
                    );
                }
                Phase::Ending(_) | Phase::Ended => {
                    rpc::complete(send, request, super::error("history_activity_ended"));
                }
            }
            return;
        }
        if self.entries.len() >= MAX_RESERVATIONS {
            rpc::complete(send, request, super::error("capacity_exceeded"));
            return;
        }
        let pending = rpc::Pending::new(
            SERVICE,
            json!({"action":"stream_activity_begin","session_id":activity_id}),
            format!("history-activity-{activity_id}-begin"),
            None,
        )
        .retry_uncertain_admission();
        self.entries.insert(
            activity_id,
            Reservation {
                rtsp_operation_id,
                phase: Phase::Beginning(pending),
                began_at: now,
                terminal_at: None,
                retry_at: now,
                callers: vec![request],
                release_callers: Vec::new(),
                release_requested: false,
                release_requested_at: None,
                completed_at: None,
            },
        );
    }

    pub(super) fn release(
        &mut self,
        request: u64,
        activity_id: &str,
        rtsp_operation_id: &str,
        now: u64,
        records: &BTreeMap<u64, Record>,
        send: &mut rpc::Send<'_>,
    ) {
        let Some(entry) = self.entries.get_mut(activity_id) else {
            rpc::complete(send, request, super::error("history_activity_not_found"));
            return;
        };
        if entry.rtsp_operation_id != rtsp_operation_id {
            rpc::complete(send, request, super::error("operation_conflict"));
            return;
        }
        if records.values().any(|record| {
            record.config.history_activity_id.as_deref() == Some(activity_id)
                && (!record.state.terminal() || record.history.has_work())
        }) {
            rpc::complete(send, request, super::error("history_activity_in_use"));
            return;
        }
        match entry.phase {
            Phase::Beginning(_) => {
                entry.release_requested = true;
                entry.release_requested_at.get_or_insert(now);
                if entry.release_callers.len() < 8 {
                    entry.release_callers.push(request);
                } else {
                    rpc::complete(send, request, super::error("capacity_exceeded"));
                }
            }
            Phase::Ending(_) if entry.release_callers.len() < 8 => {
                entry.release_callers.push(request)
            }
            Phase::Ending(_) => {
                rpc::complete(send, request, super::error("capacity_exceeded"));
            }
            Phase::Ended | Phase::Unavailable => {
                rpc::complete(
                    send,
                    request,
                    reply("released", false, activity_id, rtsp_operation_id),
                );
            }
            Phase::Reserved => {
                entry.phase = Phase::Ending(
                    rpc::Pending::new(
                        SERVICE,
                        json!({"action":"stream_activity_end","session_id":activity_id}),
                        format!("history-activity-{activity_id}-end"),
                        None,
                    )
                    .retry_uncertain_admission(),
                );
                entry.release_callers.push(request);
                entry.release_requested_at.get_or_insert(now);
                entry.retry_at = now;
            }
        }
    }

    pub(super) fn matches(&self, activity_id: &str, rtsp_operation_id: &str, now: u64) -> bool {
        self.entries.get(activity_id).is_some_and(|entry| {
            entry.rtsp_operation_id == rtsp_operation_id
                && now.saturating_sub(entry.began_at) < RESERVATION_MS
                && matches!(entry.phase, Phase::Reserved)
        })
    }

    pub(super) fn progress(
        &mut self,
        now: u64,
        records: &BTreeMap<u64, Record>,
        send: &mut rpc::Send<'_>,
    ) {
        self.prune(now);
        for (activity_id, entry) in &mut self.entries {
            let attached_active = records.values().any(|record| {
                record.config.history_activity_id.as_deref() == Some(activity_id)
                    && (!record.state.terminal() || record.history.has_work())
            });
            let attached = records
                .values()
                .any(|record| record.config.history_activity_id.as_deref() == Some(activity_id));
            if attached && !attached_active {
                entry.completed_at.get_or_insert(now);
            }
            let expired = if attached {
                entry
                    .completed_at
                    .is_some_and(|completed| now.saturating_sub(completed) >= RESERVATION_MS)
            } else {
                now.saturating_sub(entry.began_at) >= RESERVATION_MS
            };
            if expired && matches!(entry.phase, Phase::Beginning(_) | Phase::Reserved) {
                entry.release_requested = true;
                for request in entry.callers.drain(..) {
                    rpc::complete(
                        send,
                        request,
                        reply("reserved", false, activity_id, &entry.rtsp_operation_id),
                    );
                }
                for request in entry.release_callers.drain(..) {
                    rpc::complete(
                        send,
                        request,
                        super::error("history_activity_end_unconfirmed"),
                    );
                }
                let begin_settled = if let Phase::Beginning(pending) = &mut entry.phase {
                    pending.shutdown(send)
                } else {
                    true
                };
                if begin_settled {
                    entry.phase = Phase::Ending(activity_request(activity_id, false));
                    entry.retry_at = now;
                }
                // Otherwise keep polling the original Begin until settlement.
                // End must not overtake a late Begin.
            }
            let cleanup_expired = if attached {
                entry
                    .completed_at
                    .is_some_and(|completed| now.saturating_sub(completed) >= RESERVATION_MS * 2)
            } else {
                now.saturating_sub(entry.began_at) >= RESERVATION_MS * 2
            };
            if !attached_active && cleanup_expired && matches!(entry.phase, Phase::Ending(_)) {
                for request in entry.release_callers.drain(..) {
                    rpc::complete(
                        send,
                        request,
                        super::error("history_activity_end_unconfirmed"),
                    );
                }
                // Keep the exact End until its provider response proves the
                // activity retired; broker cancellation is not that proof.
            }
            if now < entry.retry_at {
                continue;
            }
            let result = match &mut entry.phase {
                Phase::Beginning(pending) | Phase::Ending(pending) => {
                    match pending.poll(send, now) {
                        rpc::Progress::Result(response) if pending.release_result(send) => {
                            Some(response)
                        }
                        rpc::Progress::Failed if pending.admission_rejected() => Some(Value::Null),
                        rpc::Progress::Failed => {
                            // Preserve the original admission uncertainty; a
                            // later rejection cannot disprove the first send.
                            pending.retry();
                            entry.retry_at = now.saturating_add(1_000);
                            None
                        }
                        rpc::Progress::Unresolved => {
                            pending.retry();
                            entry.retry_at = now.saturating_add(1_000);
                            None
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(result) = result {
                let (field, accepted) = match &entry.phase {
                    Phase::Beginning(_) => (
                        "reserved",
                        result["ok"] == true && result["reserved"] == true,
                    ),
                    Phase::Ending(_) => (
                        "released",
                        result["ok"] == true && result["ended"].is_boolean(),
                    ),
                    _ => unreachable!(),
                };
                let ended = matches!(entry.phase, Phase::Ending(_));
                entry.phase = if ended && accepted {
                    entry.terminal_at = Some(now);
                    Phase::Ended
                } else if ended {
                    entry.retry_at = now.saturating_add(1_000);
                    Phase::Ending(
                        rpc::Pending::new(
                            SERVICE,
                            json!({"action":"stream_activity_end","session_id":activity_id}),
                            format!("history-activity-{activity_id}-end"),
                            None,
                        )
                        .retry_uncertain_admission(),
                    )
                } else if accepted && entry.release_requested {
                    Phase::Ending(
                        rpc::Pending::new(
                            SERVICE,
                            json!({"action":"stream_activity_end","session_id":activity_id}),
                            format!("history-activity-{activity_id}-end"),
                            None,
                        )
                        .retry_uncertain_admission(),
                    )
                } else if accepted {
                    Phase::Reserved
                } else {
                    entry.terminal_at = Some(now);
                    Phase::Unavailable
                };
                for request in entry.callers.drain(..) {
                    rpc::complete(
                        send,
                        request,
                        reply(field, accepted, activity_id, &entry.rtsp_operation_id),
                    );
                }
                if ended || !accepted {
                    for request in entry.release_callers.drain(..) {
                        let response = if ended && !accepted {
                            super::error("history_activity_end_unconfirmed")
                        } else {
                            reply(
                                "released",
                                accepted && ended,
                                activity_id,
                                &entry.rtsp_operation_id,
                            )
                        };
                        rpc::complete(send, request, response);
                    }
                }
            } else if !entry.callers.is_empty() && now.saturating_sub(entry.began_at) >= 10_000 {
                let field = if matches!(entry.phase, Phase::Beginning(_)) {
                    "reserved"
                } else {
                    "released"
                };
                for request in entry.callers.drain(..) {
                    rpc::complete(
                        send,
                        request,
                        reply(field, false, activity_id, &entry.rtsp_operation_id),
                    );
                }
            }
            if entry
                .release_requested_at
                .is_some_and(|started| now.saturating_sub(started) >= 10_000)
            {
                for request in entry.release_callers.drain(..) {
                    rpc::complete(
                        send,
                        request,
                        super::error("history_activity_end_unconfirmed"),
                    );
                }
            }
        }
    }

    fn prune(&mut self, now: u64) {
        self.entries.retain(|_, entry| {
            entry
                .terminal_at
                .is_none_or(|ended| now.saturating_sub(ended) < RESERVATION_MS)
        });
    }

    pub(super) fn shutdown(&mut self, _send: &mut rpc::Send<'_>) -> bool {
        self.entries.values().all(|entry| match &entry.phase {
            Phase::Ended | Phase::Unavailable => true,
            Phase::Beginning(pending) => !pending.was_sent(),
            Phase::Reserved | Phase::Ending(_) => false,
        })
    }
}

fn reply(field: &str, accepted: bool, activity_id: &str, rtsp_operation_id: &str) -> Value {
    let mut result =
        json!({"ok":true,"activity_id":activity_id,"rtsp_operation_id":rtsp_operation_id});
    result[field] = json!(accepted);
    result
}

fn activity_request(activity_id: &str, beginning: bool) -> rpc::Pending {
    let action = if beginning {
        "stream_activity_begin"
    } else {
        "stream_activity_end"
    };
    let suffix = if beginning { "begin" } else { "end" };
    rpc::Pending::new(
        SERVICE,
        json!({"action":action,"session_id":activity_id}),
        format!("history-activity-{activity_id}-{suffix}"),
        None,
    )
    .retry_uncertain_admission()
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::{Command, ModuleResult, Response};

    #[test]
    fn repeated_rejections_do_not_clear_an_uncertain_begin() {
        let mut activities = HistoryActivities::default();
        let mut sends = 0;
        let mut send = |command: Command| -> ModuleResult<Response> {
            match command {
                Command::Request { .. } => {
                    sends += 1;
                    if sends == 1 {
                        Err("lost admission".into())
                    } else {
                        Ok(Response::Error {
                            code: "unavailable".into(),
                            message: String::new(),
                        })
                    }
                }
                Command::Complete { .. } => Ok(Response::Ok),
                other => panic!("unexpected command: {other:?}"),
            }
        };
        activities.reserve(3, "activity".into(), "rtsp".into(), 0, &mut send);
        for now in [0, 1, 1_001, 2_001, 3_001] {
            activities.progress(now, &BTreeMap::new(), &mut send);
        }
        assert!(sends >= 3);
        assert!(activities.has_active_reservations());
        assert!(matches!(
            activities.entries["activity"].phase,
            Phase::Beginning(_)
        ));
    }

    #[test]
    fn expired_failed_end_retains_activity_ownership() {
        let mut activities = HistoryActivities::default();
        activities.entries.insert(
            "activity".into(),
            Reservation {
                rtsp_operation_id: "rtsp".into(),
                phase: Phase::Ending(activity_request("activity", false)),
                began_at: 0,
                terminal_at: None,
                retry_at: 0,
                callers: Vec::new(),
                release_callers: Vec::new(),
                release_requested: true,
                release_requested_at: Some(0),
                completed_at: None,
            },
        );
        let mut send = |command| match command {
            Command::Request { .. } => Ok(Response::Accepted { id: 7 }),
            Command::TakeResult { .. } => Ok(Response::Result {
                payload: json!({"ok":false}),
            }),
            Command::ReleaseResult { .. } => Ok(Response::Ok),
            other => panic!("unexpected command: {other:?}"),
        };
        for now in [600_001, 600_002, 601_003, 601_004, 901_005] {
            activities.progress(now, &BTreeMap::new(), &mut send);
        }
        assert!(activities.has_active_reservations());
        assert!(activities.contains("activity"));
    }

    #[test]
    fn ambiguous_begin_survives_retry_rejection_until_end() {
        let mut activities = HistoryActivities::default();
        let mut admissions = 0;
        let mut actions = Vec::new();
        let mut send = |command: Command| -> ModuleResult<Response> {
            match command {
                Command::Request { payload, .. } => {
                    let action = payload["action"].as_str().unwrap().to_owned();
                    actions.push(action.clone());
                    if action == "stream_activity_begin" {
                        admissions += 1;
                        match admissions {
                            1..=3 => Err("lost admission".into()),
                            4..=7 => Ok(Response::Error {
                                code: "unavailable".into(),
                                message: String::new(),
                            }),
                            _ => Ok(Response::Accepted { id: 1 }),
                        }
                    } else {
                        Ok(Response::Accepted { id: 2 })
                    }
                }
                Command::TakeResult { request } => Ok(Response::Result {
                    payload: if request == 1 {
                        json!({"ok":true,"reserved":true})
                    } else {
                        json!({"ok":true,"ended":true})
                    },
                }),
                Command::Cancel { .. }
                | Command::ReleaseResult { .. }
                | Command::Complete { .. } => Ok(Response::Ok),
                other => panic!("unexpected command: {other:?}"),
            }
        };
        let records = BTreeMap::new();
        activities.reserve(3, "activity".into(), "rtsp".into(), 0, &mut send);
        activities.release(4, "activity", "rtsp", 0, &records, &mut send);
        for now in 0..=4 {
            activities.progress(now, &records, &mut send);
        }
        assert!(activities.has_active_reservations());
        for now in (1..=15).map(|n| n * 1_000) {
            activities.progress(now, &records, &mut send);
        }
        assert!(!activities.has_active_reservations());
        assert_eq!(
            actions
                .iter()
                .filter(|action| *action == "stream_activity_end")
                .count(),
            1
        );
        assert_eq!(
            actions.last().map(String::as_str),
            Some("stream_activity_end")
        );
    }
}
