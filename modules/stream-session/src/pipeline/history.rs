use super::rpc;
use crate::model::StartConfig;
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

const SERVICE: &str = "amaterasu.session-history/1";
const REQUEST_TIMEOUT_MS: u64 = 5_000;

enum Phase {
    NotStarted,
    BeginQueued,
    BeginPending { request: rpc::Pending, sent_at: u64 },
    BeginUncertain { request: rpc::Pending },
    Begun,
    EndQueued,
    EndPending { request: rpc::Pending, sent_at: u64 },
    EndUncertain { request: rpc::Pending },
    Complete,
}

pub(super) struct HistoryRequest {
    action: Action,
    operation_id: String,
    payload: Value,
}

#[derive(Clone, Copy)]
enum Action {
    Begin,
    End,
}

pub(super) struct SessionHistory {
    phase: Phase,
    started_at_unix: Option<i64>,
    ended_at_unix: Option<i64>,
}

impl SessionHistory {
    pub(super) fn new() -> Self {
        Self {
            phase: Phase::NotStarted,
            started_at_unix: None,
            ended_at_unix: None,
        }
    }

    pub(super) fn activated(&mut self) {
        if matches!(&self.phase, Phase::NotStarted) {
            self.started_at_unix = Some(unix_time_seconds());
            self.phase = Phase::BeginQueued;
        }
    }

    pub(super) fn teardown_confirmed(&mut self) {
        if matches!(&self.phase, Phase::NotStarted) {
            self.phase = Phase::Complete;
            return;
        }
        if self.ended_at_unix.is_none() {
            self.ended_at_unix = Some(unix_time_seconds());
        }
        if matches!(&self.phase, Phase::Begun) {
            self.phase = Phase::EndQueued;
        } else if matches!(&self.phase, Phase::BeginUncertain { .. }) {
            self.phase = Phase::Complete;
        }
    }

    pub(super) fn has_work(&self) -> bool {
        matches!(
            &self.phase,
            Phase::BeginQueued
                | Phase::BeginPending { .. }
                | Phase::BeginUncertain { .. }
                | Phase::EndQueued
                | Phase::EndPending { .. }
                | Phase::EndUncertain { .. }
        )
    }

    pub(super) fn queued_request(
        &self,
        record_id: u64,
        owner: &str,
        start: &StartConfig,
    ) -> Option<HistoryRequest> {
        let session_id = history_session_id(record_id);
        match &self.phase {
            Phase::BeginQueued => {
                let metadata = &start.configuration;
                Some(HistoryRequest {
                    action: Action::Begin,
                    operation_id: history_operation_id(record_id, "begin"),
                    payload: json!({
                        "action": "session_begin",
                        "metadata": {
                            "session_id": session_id,
                            "protocol": "sunshine-modern",
                            "client_name": bounded_text(owner, 128),
                            "device_name": bounded_text(
                                start.device_name.as_deref().unwrap_or(&start.output),
                                128,
                            ),
                            "app_name": "",
                            "width": metadata.width,
                            "height": metadata.height,
                            "target_fps": metadata.fps,
                            "target_bitrate_kbps": metadata.bitrate / 1000,
                            "codec": "",
                        },
                        "started_at_unix": self.started_at_unix,
                    }),
                })
            }
            Phase::EndQueued => Some(HistoryRequest {
                action: Action::End,
                operation_id: history_operation_id(record_id, "end"),
                payload: json!({
                    "action": "session_end",
                    "session_id": session_id,
                    "ended_at_unix": self.ended_at_unix,
                }),
            }),
            _ => None,
        }
    }

    pub(super) fn progress(
        &mut self,
        request: Option<HistoryRequest>,
        now: u64,
        send: &mut rpc::Send<'_>,
    ) {
        let phase = std::mem::replace(&mut self.phase, Phase::Complete);
        match phase {
            Phase::BeginQueued => {
                let Some(request) = request else {
                    self.phase = Phase::BeginQueued;
                    return;
                };
                let mut pending =
                    rpc::Pending::new(SERVICE, request.payload, request.operation_id, None);
                debug_assert!(matches!(request.action, Action::Begin));
                let progress = poll_and_release_result(&mut pending, send, now);
                self.progress_begin(progress, pending, now);
            }
            Phase::BeginPending {
                mut request,
                sent_at,
            } => {
                if now.saturating_sub(sent_at) >= REQUEST_TIMEOUT_MS {
                    request.cancel();
                    self.phase = Phase::BeginUncertain { request };
                } else {
                    let progress = poll_and_release_result(&mut request, send, now);
                    self.progress_begin(progress, request, sent_at);
                }
            }
            Phase::BeginUncertain { mut request } => {
                self.phase = match poll_and_release_result(&mut request, send, now) {
                    rpc::Progress::Waiting => Phase::BeginUncertain { request },
                    rpc::Progress::Result(_)
                    | rpc::Progress::Failed
                    | rpc::Progress::Unresolved => Phase::Complete,
                };
            }
            Phase::Begun => self.phase = Phase::Begun,
            Phase::EndQueued => {
                let Some(request) = request else {
                    self.phase = Phase::EndQueued;
                    return;
                };
                let mut pending =
                    rpc::Pending::new(SERVICE, request.payload, request.operation_id, None);
                let progress = poll_and_release_result(&mut pending, send, now);
                self.progress_end(progress, pending, now);
            }
            Phase::EndPending {
                mut request,
                sent_at,
            } => {
                if now.saturating_sub(sent_at) >= REQUEST_TIMEOUT_MS {
                    request.cancel();
                    self.phase = Phase::EndUncertain { request };
                } else {
                    let progress = poll_and_release_result(&mut request, send, now);
                    self.progress_end(progress, request, sent_at);
                }
            }
            Phase::EndUncertain { mut request } => {
                self.phase = match poll_and_release_result(&mut request, send, now) {
                    rpc::Progress::Waiting => Phase::EndUncertain { request },
                    rpc::Progress::Result(_)
                    | rpc::Progress::Failed
                    | rpc::Progress::Unresolved => Phase::Complete,
                };
            }
            Phase::NotStarted => self.phase = Phase::NotStarted,
            Phase::Complete => self.phase = Phase::Complete,
        }
    }

    fn progress_begin(&mut self, progress: rpc::Progress, request: rpc::Pending, sent_at: u64) {
        self.phase = match progress {
            rpc::Progress::Waiting => Phase::BeginPending { request, sent_at },
            rpc::Progress::Result(response) if response["ok"] == true => {
                if self.ended_at_unix.is_some() {
                    Phase::EndQueued
                } else {
                    Phase::Begun
                }
            }
            rpc::Progress::Result(_) => Phase::Complete,
            rpc::Progress::Failed => Phase::Complete,
            rpc::Progress::Unresolved => Phase::BeginUncertain { request },
        };
    }

    fn progress_end(&mut self, progress: rpc::Progress, request: rpc::Pending, sent_at: u64) {
        self.phase = match progress {
            rpc::Progress::Waiting => Phase::EndPending { request, sent_at },
            rpc::Progress::Result(_) | rpc::Progress::Failed | rpc::Progress::Unresolved => {
                Phase::Complete
            }
        };
    }

    pub(super) fn shutdown(&mut self, send: &mut rpc::Send<'_>) -> bool {
        match &mut self.phase {
            Phase::BeginPending { request, .. }
            | Phase::BeginUncertain { request }
            | Phase::EndPending { request, .. }
            | Phase::EndUncertain { request } => request.shutdown(send),
            _ => true,
        }
    }
}

fn history_session_id(record_id: u64) -> String {
    format!("stream-session-{record_id:016x}")
}

fn poll_and_release_result(
    request: &mut rpc::Pending,
    send: &mut rpc::Send<'_>,
    now: u64,
) -> rpc::Progress {
    let progress = request.poll(send, now);
    if matches!(&progress, rpc::Progress::Result(_)) {
        for _ in 0..3 {
            if request.release_result(send) {
                break;
            }
        }
    }
    progress
}

fn history_operation_id(record_id: u64, action: &str) -> String {
    format!("ss-{record_id:x}-history-{action}")
}

fn bounded_text(value: &str, maximum_bytes: usize) -> String {
    let mut bounded = String::new();
    for character in value.chars() {
        if bounded.len() + character.len_utf8() > maximum_bytes {
            break;
        }
        bounded.push(character);
    }
    bounded
}

fn unix_time_seconds() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_secs()).unwrap_or(i64::MAX),
    }
}
