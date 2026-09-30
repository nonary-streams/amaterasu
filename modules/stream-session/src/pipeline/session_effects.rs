use super::rpc;
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};

const UPDATER: &str = "amaterasu.updater/1";
const SYSTEM_TRAY: &str = "amaterasu.system-tray/1";

struct QueuedOperation {
    payload: Value,
    operation_id: String,
}

#[derive(Default)]
struct OperationQueue {
    queued: VecDeque<QueuedOperation>,
    pending: Option<rpc::Pending>,
}

impl OperationQueue {
    fn push(&mut self, payload: Value, operation_id: String) {
        self.queued.push_back(QueuedOperation {
            payload,
            operation_id,
        });
    }

    fn progress(&mut self, service: &str, now: u64, send: &mut rpc::Send<'_>) {
        if self.pending.is_none()
            && let Some(operation) = self.queued.pop_front()
        {
            self.pending = Some(
                rpc::Pending::new(service, operation.payload, operation.operation_id, None)
                    .retry_uncertain_admission(),
            );
        }
        let Some(mut pending) = self.pending.take() else {
            return;
        };
        match pending.poll(send, now) {
            rpc::Progress::Result(_) => {
                if !pending.release_result(send) {
                    self.pending = Some(pending);
                }
            }
            rpc::Progress::Failed if pending.admission_rejected() => {}
            rpc::Progress::Failed | rpc::Progress::Unresolved => {
                pending.retry();
                self.pending = Some(pending);
            }
            rpc::Progress::Waiting => self.pending = Some(pending),
        }
    }
}

#[derive(Default)]
pub(super) struct SessionEffects {
    session_id: Option<String>,
    activated: bool,
    ended: bool,
    vigem_missing_notified: bool,
    updater: OperationQueue,
    tray: OperationQueue,
}

impl SessionEffects {
    pub fn activated(&mut self, record_id: u64) {
        if self.activated {
            return;
        }
        let session_id = format!("stream-session-{record_id:016x}");
        self.session_id = Some(session_id.clone());
        self.updater.push(
            json!({"action":"stream_started","session_id":session_id}),
            format!("ss-{record_id:016x}-updater-start"),
        );
        self.activated = true;
    }

    pub fn teardown_confirmed(&mut self, record_id: u64) {
        if !self.activated || self.ended {
            return;
        }
        let Some(session_id) = self.session_id.as_ref() else {
            return;
        };
        self.updater.push(
            json!({"action":"stream_stopped","session_id":session_id}),
            format!("ss-{record_id:016x}-updater-stop"),
        );
        self.ended = true;
    }

    pub fn vigem_missing(&mut self, record_id: u64) {
        if self.vigem_missing_notified {
            return;
        }
        self.tray.push(
            json!({"action":"notify_vigem_missing"}),
            format!("ss-{record_id:016x}-tray-vigem-missing"),
        );
        self.vigem_missing_notified = true;
    }

    pub fn progress(&mut self, now: u64, send: &mut rpc::Send<'_>) {
        self.updater.progress(UPDATER, now, send);
        self.tray.progress(SYSTEM_TRAY, now, send);
    }
}

#[derive(Default)]
pub(super) struct TrayActivityEffects {
    active_sessions: BTreeSet<u64>,
    activity: OperationQueue,
}

impl TrayActivityEffects {
    pub fn activated(&mut self, record_id: u64) {
        if self.active_sessions.insert(record_id) && self.active_sessions.len() == 1 {
            self.activity.push(
                json!({
                    "action":"activity_update",
                    "update":{"state":"playing","app_name":"Active stream"}
                }),
                format!("ss-{record_id:016x}-tray-playing"),
            );
        }
    }

    pub fn teardown_confirmed(&mut self, record_id: u64) {
        if self.active_sessions.remove(&record_id) && self.active_sessions.is_empty() {
            self.activity.push(
                json!({
                    "action":"activity_update",
                    "update":{"state":"stopped","app_name":"Active stream"}
                }),
                format!("ss-{record_id:016x}-tray-stopped"),
            );
        }
    }

    pub fn progress(&mut self, now: u64, send: &mut rpc::Send<'_>) {
        self.activity.progress(SYSTEM_TRAY, now, send);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_effects_queue_balanced_updater_and_vigem_transitions_once() {
        let mut effects = SessionEffects::default();
        effects.activated(7);
        effects.activated(7);
        effects.vigem_missing(7);
        effects.vigem_missing(7);
        effects.teardown_confirmed(7);
        effects.teardown_confirmed(7);

        let updater: Vec<_> = effects
            .updater
            .queued
            .iter()
            .map(|operation| operation.payload.clone())
            .collect();
        assert_eq!(updater.len(), 2);
        assert_eq!(updater[0]["action"], "stream_started");
        assert_eq!(updater[1]["action"], "stream_stopped");
        assert_eq!(updater[0]["session_id"], "stream-session-0000000000000007");
        assert_eq!(updater[1]["session_id"], updater[0]["session_id"]);

        let tray: Vec<_> = effects
            .tray
            .queued
            .iter()
            .map(|operation| operation.payload.clone())
            .collect();
        assert_eq!(tray.len(), 1);
        assert_eq!(tray[0]["action"], "notify_vigem_missing");
    }

    #[test]
    fn global_tray_activity_stays_playing_until_the_last_stream_stops() {
        let mut activity = TrayActivityEffects::default();
        activity.activated(7);
        activity.activated(8);
        activity.activated(7);
        assert_eq!(activity.activity.queued.len(), 1);
        assert_eq!(
            activity.activity.queued[0].payload["update"]["state"],
            "playing"
        );

        activity.teardown_confirmed(7);
        activity.teardown_confirmed(7);
        assert_eq!(activity.active_sessions, BTreeSet::from([8]));
        assert_eq!(activity.activity.queued.len(), 1);

        activity.teardown_confirmed(8);
        assert!(activity.active_sessions.is_empty());
        assert_eq!(activity.activity.queued.len(), 2);
        assert_eq!(
            activity.activity.queued[1].payload["update"]["state"],
            "stopped"
        );
    }
}
