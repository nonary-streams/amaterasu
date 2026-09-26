use super::rpc;
use crate::model::StartConfig;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

const SERVICE: &str = "amaterasu.session-history/1";
const REQUEST_TIMEOUT_MS: u64 = 5_000;
const SAMPLE_INTERVAL_MS: u64 = 1_000;
const MAX_QUEUED_UPDATES: usize = 8;

#[derive(Clone, Copy)]
enum Action {
    Begin,
    Update,
    End,
}

pub(super) struct HistoryRequest {
    action: Action,
    operation_id: String,
    payload: Value,
}

struct QueuedUpdate {
    request: HistoryRequest,
    sample_range_start: Option<SamplePoint>,
}

#[derive(Clone, Copy)]
struct SamplePoint {
    at_ms: u64,
    frames_sent: u64,
    bytes_sent: u64,
    dropped_frames: u64,
}

enum Phase {
    NotStarted,
    BeginQueued,
    Pending {
        action: Action,
        request: rpc::Pending,
        sent_at: u64,
    },
    Uncertain {
        action: Action,
        request: rpc::Pending,
    },
    Begun,
    Complete,
}

pub(super) struct SessionHistory {
    record_id: u64,
    session_id: String,
    phase: Phase,
    started_at_unix: Option<i64>,
    ended_at_unix: Option<i64>,
    updates: VecDeque<QueuedUpdate>,
    next_update_id: u64,
    frames_sent: u64,
    bytes_sent: u64,
    dropped_frames: u64,
    last_sample_scheduled: Option<SamplePoint>,
    stopping_queued: bool,
    terminal_queued: bool,
}

impl SessionHistory {
    pub(super) fn new(record_id: u64, activity_id: Option<String>) -> Self {
        Self {
            record_id,
            session_id: activity_id.unwrap_or_else(|| history_session_id(record_id)),
            phase: Phase::NotStarted,
            started_at_unix: None,
            ended_at_unix: None,
            updates: VecDeque::new(),
            next_update_id: 0,
            frames_sent: 0,
            bytes_sent: 0,
            dropped_frames: 0,
            last_sample_scheduled: None,
            stopping_queued: false,
            terminal_queued: false,
        }
    }

    pub(super) fn activated(&mut self, now: u64) {
        if !matches!(&self.phase, Phase::NotStarted) {
            return;
        }
        self.started_at_unix = Some(unix_time_seconds());
        self.last_sample_scheduled = Some(self.sample_point(now));
        self.phase = Phase::BeginQueued;
        self.queue_event(
            "telemetry_limits",
            json!({
                "counter_scope":"complete video access units and encoded audio/video payload bytes accepted by the media-transport local queue",
                "dropped_frames_scope":"forward video sequence gaps between accepted access units; end-to-end frame loss is not observable",
                "encode_latency_ms":"not measured; the video ABI exposes no encoder timing",
                "zero_duration_rates":"actual_fps and actual_bitrate_kbps are numeric zero when no monotonic millisecond elapsed; unknown is not representable in the history schema",
                "audio_payload_bytes_included":true,
                "transport_acceptance_is_client_delivery":false
            }),
        );
    }

    pub(super) fn observe_media(
        &mut self,
        accepted_video_access_units: u64,
        accepted_media_payload_bytes: u64,
        observed_video_sequence_gaps: u64,
    ) {
        self.frames_sent = self.frames_sent.saturating_add(accepted_video_access_units);
        self.bytes_sent = self.bytes_sent.saturating_add(accepted_media_payload_bytes);
        self.dropped_frames = self
            .dropped_frames
            .saturating_add(observed_video_sequence_gaps);
    }

    #[cfg(test)]
    pub(super) fn observed_counters(&self) -> (u64, u64, u64) {
        (self.frames_sent, self.bytes_sent, self.dropped_frames)
    }

    pub(super) fn sample_if_due(&mut self, now: u64) {
        if !matches!(&self.phase, Phase::Begun) || self.ended_at_unix.is_some() {
            return;
        }

        let queued_sample = self
            .updates
            .iter()
            .position(|update| update.sample_range_start.is_some());
        let range_start = queued_sample
            .and_then(|index| self.updates.get(index))
            .and_then(|update| update.sample_range_start)
            .or(self.last_sample_scheduled);
        let Some(range_start) = range_start else {
            return;
        };
        let elapsed_ms = now.saturating_sub(range_start.at_ms);
        if elapsed_ms < SAMPLE_INTERVAL_MS {
            return;
        }
        if let Some(index) = queued_sample {
            self.updates.remove(index);
        }

        self.schedule_sample(now, range_start, elapsed_ms);
    }

    pub(super) fn stopping(&mut self, reason: &str) {
        if self.started_at_unix.is_none() || self.stopping_queued || self.terminal_queued {
            return;
        }
        self.stopping_queued = true;
        self.queue_event(
            "stream_stopping",
            json!({"state":"stopping","reason":bounded_text(reason, 64)}),
        );
    }

    pub(super) fn teardown_confirmed(&mut self, now: u64, outcome: &str, reason: Option<&str>) {
        if self.started_at_unix.is_none() {
            self.phase = Phase::Complete;
            self.updates.clear();
            return;
        }
        self.sample_on_teardown(now);
        if self.ended_at_unix.is_none() {
            self.ended_at_unix = Some(unix_time_seconds());
        }
        if !self.terminal_queued {
            self.terminal_queued = true;
            let mut payload = json!({"state":bounded_text(outcome, 32)});
            if let Some(reason) = reason {
                payload["reason"] = json!(bounded_text(reason, 64));
            }
            let event_type = match outcome {
                "stopped" => "stream_stopped",
                "cancelled" => "stream_cancelled",
                _ => "stream_failed",
            };
            self.queue_event(event_type, payload);
        }
    }

    pub(super) fn has_work(&self) -> bool {
        match &self.phase {
            Phase::BeginQueued | Phase::Pending { .. } | Phase::Uncertain { .. } => true,
            Phase::Begun => !self.updates.is_empty() || self.ended_at_unix.is_some(),
            Phase::NotStarted | Phase::Complete => false,
        }
    }

    pub(super) fn queued_request(
        &self,
        owner: &str,
        start: &StartConfig,
    ) -> Option<HistoryRequest> {
        match &self.phase {
            Phase::BeginQueued => {
                let metadata = &start.configuration;
                Some(HistoryRequest {
                    action: Action::Begin,
                    operation_id: history_operation_id(self.record_id, "begin"),
                    payload: json!({
                        "action":"session_begin",
                        "metadata":{
                            "session_id":self.session_id,
                            "protocol":"sunshine-modern",
                            "client_name":bounded_text(owner, 128),
                            "device_name":bounded_text(
                                start.device_name.as_deref().unwrap_or(&start.output),
                                128
                            ),
                            "app_name":"",
                            "width":metadata.width,
                            "height":metadata.height,
                            "target_fps":metadata.fps,
                            "target_bitrate_kbps":metadata.bitrate / 1000,
                            "codec":""
                        },
                        "started_at_unix":self.started_at_unix
                    }),
                })
            }
            Phase::Begun => self
                .updates
                .front()
                .map(|update| HistoryRequest {
                    action: Action::Update,
                    operation_id: update.request.operation_id.clone(),
                    payload: update.request.payload.clone(),
                })
                .or_else(|| self.ended_at_unix.map(|_| self.end_request())),
            Phase::NotStarted
            | Phase::Pending { .. }
            | Phase::Uncertain { .. }
            | Phase::Complete => None,
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
                if let Some(request) = request {
                    let pending =
                        rpc::Pending::new(SERVICE, request.payload, request.operation_id, None);
                    self.progress_pending(request.action, pending, now, now, send);
                } else {
                    self.phase = Phase::BeginQueued;
                }
            }
            Phase::Begun => {
                if let Some(request) = request {
                    self.remove_in_flight_update(&request);
                    let pending =
                        rpc::Pending::new(SERVICE, request.payload, request.operation_id, None);
                    self.progress_pending(request.action, pending, now, now, send);
                } else {
                    self.phase = Phase::Begun;
                }
            }
            Phase::Pending {
                action,
                mut request,
                sent_at,
            } => match poll_and_release_result(&mut request, send, now) {
                rpc::Progress::Waiting if now.saturating_sub(sent_at) >= REQUEST_TIMEOUT_MS => {
                    request.cancel();
                    self.phase = Phase::Uncertain { action, request };
                }
                rpc::Progress::Waiting => {
                    self.phase = Phase::Pending {
                        action,
                        request,
                        sent_at,
                    };
                }
                rpc::Progress::Unresolved => {
                    request.retry();
                    request.cancel();
                    self.phase = Phase::Uncertain { action, request };
                }
                progress => self.finish_request(action, progress, request, false, send),
            },
            Phase::Uncertain {
                action,
                mut request,
            } => match poll_and_release_result(&mut request, send, now) {
                rpc::Progress::Waiting => self.phase = Phase::Uncertain { action, request },
                progress @ (rpc::Progress::Failed | rpc::Progress::Unresolved) => {
                    request.retry();
                    if matches!(progress, rpc::Progress::Unresolved) {
                        self.phase = Phase::Uncertain { action, request };
                    } else {
                        self.finish_request(action, progress, request, true, send);
                    }
                }
                progress => self.finish_request(action, progress, request, true, send),
            },
            Phase::NotStarted => self.phase = Phase::NotStarted,
            Phase::Complete => self.phase = Phase::Complete,
        }
    }

    fn progress_pending(
        &mut self,
        action: Action,
        mut request: rpc::Pending,
        now: u64,
        sent_at: u64,
        send: &mut rpc::Send<'_>,
    ) {
        match poll_and_release_result(&mut request, send, now) {
            rpc::Progress::Waiting => {
                self.phase = Phase::Pending {
                    action,
                    request,
                    sent_at,
                }
            }
            progress => self.finish_request(action, progress, request, false, send),
        }
    }

    fn finish_request(
        &mut self,
        action: Action,
        progress: rpc::Progress,
        request: rpc::Pending,
        uncertain: bool,
        _send: &mut rpc::Send<'_>,
    ) {
        let uncertain = uncertain || request.was_admitted();
        match action {
            Action::Begin => match progress {
                rpc::Progress::Result(response) if response["ok"] == true => {
                    self.phase = Phase::Begun;
                }
                rpc::Progress::Failed if uncertain => {
                    // The call may have committed before cancellation. Continue with
                    // ordered transitions and an idempotent end to avoid an orphan.
                    self.phase = Phase::Begun;
                }
                rpc::Progress::Waiting => self.phase = Phase::BeginQueued,
                rpc::Progress::Result(_) | rpc::Progress::Failed | rpc::Progress::Unresolved => {
                    self.updates.clear();
                    self.phase = Phase::Complete;
                }
            },
            Action::Update => {
                self.phase = Phase::Begun;
            }
            Action::End => self.phase = Phase::Complete,
        }
    }

    pub(super) fn shutdown(&mut self, send: &mut rpc::Send<'_>) -> bool {
        let phase = std::mem::replace(&mut self.phase, Phase::Complete);
        match phase {
            Phase::Pending {
                action,
                mut request,
                ..
            }
            | Phase::Uncertain {
                action,
                mut request,
            } => {
                let may_have_committed = request.was_sent();
                if !request.shutdown(send) {
                    self.phase = Phase::Uncertain { action, request };
                    return false;
                }
                if matches!(action, Action::End)
                    || self.ended_at_unix.is_some()
                        && (matches!(action, Action::Update)
                            || matches!(action, Action::Begin) && may_have_committed)
                {
                    self.phase = Phase::Begun;
                }
            }
            Phase::Begun => self.phase = Phase::Begun,
            Phase::NotStarted | Phase::BeginQueued | Phase::Complete => {}
        }

        if self.ended_at_unix.is_none() || !matches!(&self.phase, Phase::Begun) {
            self.updates.clear();
            self.phase = Phase::Complete;
            return true;
        }

        while let Some(update) = self.updates.pop_front() {
            match settle_history_request(update.request, send) {
                Ok(()) => {}
                Err(request) => {
                    self.phase = Phase::Uncertain {
                        action: Action::Update,
                        request: *request,
                    };
                    return false;
                }
            }
        }

        match settle_history_request(self.end_request(), send) {
            Ok(()) => {
                self.phase = Phase::Complete;
                true
            }
            Err(request) => {
                self.phase = Phase::Uncertain {
                    action: Action::End,
                    request: *request,
                };
                false
            }
        }
    }

    fn queue_event(&mut self, event_type: &str, payload: Value) {
        let sequence = self.next_update_sequence();
        self.enqueue_update(QueuedUpdate {
            request: HistoryRequest {
                action: Action::Update,
                operation_id: history_update_operation_id(self.record_id, sequence),
                payload: json!({
                    "action":"record_event",
                    "event":{
                        "session_id":self.session_id,
                        "recorded_at_unix":unix_time_seconds(),
                        "event_type":event_type,
                        "payload":payload
                    }
                }),
            },
            sample_range_start: None,
        });
    }

    fn remove_in_flight_update(&mut self, request: &HistoryRequest) {
        if !matches!(request.action, Action::Update) {
            return;
        }
        if self
            .updates
            .front()
            .is_some_and(|update| update.request.operation_id == request.operation_id)
        {
            self.updates.pop_front();
        }
    }

    fn sample_on_teardown(&mut self, now: u64) {
        let Some(last_scheduled) = self.last_sample_scheduled else {
            return;
        };
        if self.frames_sent == last_scheduled.frames_sent
            && self.bytes_sent == last_scheduled.bytes_sent
            && self.dropped_frames == last_scheduled.dropped_frames
        {
            return;
        }
        let range_start = self
            .updates
            .iter()
            .find_map(|update| update.sample_range_start)
            .unwrap_or(last_scheduled);
        let elapsed_ms = now.saturating_sub(range_start.at_ms);
        self.schedule_sample(now, range_start, elapsed_ms);
    }

    fn schedule_sample(&mut self, now: u64, range_start: SamplePoint, elapsed_ms: u64) {
        let elapsed_seconds = elapsed_ms as f64 / 1000.0;
        let frame_delta = self.frames_sent.saturating_sub(range_start.frames_sent);
        let byte_delta = self.bytes_sent.saturating_sub(range_start.bytes_sent);
        let (actual_fps, actual_bitrate_kbps) = if elapsed_seconds > 0.0 {
            (
                frame_delta as f64 / elapsed_seconds,
                byte_delta as f64 * 8.0 / elapsed_seconds / 1000.0,
            )
        } else {
            (0.0, 0.0)
        };
        let actual_fps = finite_or_zero(actual_fps);
        let actual_bitrate_kbps = finite_or_zero(actual_bitrate_kbps);
        let point = self.sample_point(now);
        self.last_sample_scheduled = Some(point);
        let sequence = self.next_update_sequence();
        let request = HistoryRequest {
            action: Action::Update,
            operation_id: history_update_operation_id(self.record_id, sequence),
            payload: json!({
                "action":"record_sample",
                "sample":{
                    "session_id":self.session_id,
                    "recorded_at_unix":unix_time_seconds(),
                    "frames_sent":sqlite_counter(self.frames_sent),
                    "bytes_sent":sqlite_counter(self.bytes_sent),
                    "dropped_frames":sqlite_counter(self.dropped_frames),
                    "actual_fps":actual_fps,
                    "actual_bitrate_kbps":actual_bitrate_kbps,
                    "encode_latency_ms":0.0
                }
            }),
        };
        self.enqueue_update(QueuedUpdate {
            request,
            sample_range_start: Some(range_start),
        });
    }

    fn enqueue_update(&mut self, mut update: QueuedUpdate) {
        if update.sample_range_start.is_some()
            && let Some(index) = self
                .updates
                .iter()
                .position(|queued| queued.sample_range_start.is_some())
        {
            let previous = self.updates.remove(index).expect("located queued sample");
            update.sample_range_start = previous.sample_range_start;
        }
        while self.updates.len() >= MAX_QUEUED_UPDATES {
            if let Some(index) = self
                .updates
                .iter()
                .position(|queued| queued.sample_range_start.is_some())
            {
                self.updates.remove(index);
            } else if update.sample_range_start.is_some() {
                return;
            } else {
                // The state machine emits at most four lifecycle events per session.
                return;
            }
        }
        self.updates.push_back(update);
    }

    fn sample_point(&self, now: u64) -> SamplePoint {
        SamplePoint {
            at_ms: now,
            frames_sent: self.frames_sent,
            bytes_sent: self.bytes_sent,
            dropped_frames: self.dropped_frames,
        }
    }

    fn end_request(&self) -> HistoryRequest {
        let ended_at_unix = self
            .ended_at_unix
            .expect("session end follows confirmed teardown");
        HistoryRequest {
            action: Action::End,
            operation_id: history_operation_id(self.record_id, "end"),
            payload: json!({
                "action":"session_end",
                "session_id":self.session_id,
                "ended_at_unix":ended_at_unix
            }),
        }
    }

    fn next_update_sequence(&mut self) -> u64 {
        self.next_update_id = self.next_update_id.saturating_add(1);
        self.next_update_id
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
    match request.poll(send, now) {
        rpc::Progress::Result(payload) => {
            if request.release_result(send) {
                rpc::Progress::Result(payload)
            } else {
                rpc::Progress::Waiting
            }
        }
        progress => progress,
    }
}

fn settle_history_request(
    request: HistoryRequest,
    send: &mut rpc::Send<'_>,
) -> Result<(), Box<rpc::Pending>> {
    let mut pending = rpc::Pending::new(SERVICE, request.payload, request.operation_id, None);
    for _ in 0..4 {
        match poll_and_release_result(&mut pending, send, u64::MAX) {
            rpc::Progress::Result(_) | rpc::Progress::Failed => return Ok(()),
            rpc::Progress::Waiting => {}
            rpc::Progress::Unresolved => {
                pending.retry();
                return if pending.shutdown(send) {
                    Ok(())
                } else {
                    Err(Box::new(pending))
                };
            }
        }
    }
    if pending.shutdown(send) {
        Ok(())
    } else {
        Err(Box::new(pending))
    }
}

fn history_operation_id(record_id: u64, action: &str) -> String {
    format!("ss-{record_id:x}-history-{action}")
}

fn history_update_operation_id(record_id: u64, sequence: u64) -> String {
    format!("ss-{record_id:x}-history-update-{sequence:x}")
}

fn sqlite_counter(value: u64) -> u64 {
    value.min(i64::MAX as u64)
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
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

#[cfg(test)]
mod tests {
    use super::{MAX_QUEUED_UPDATES, Phase, SamplePoint, SessionHistory, sqlite_counter};
    use serde_json::json;

    #[test]
    fn cumulative_counters_fit_the_history_sqlite_integer_range() {
        let maximum = i64::MAX as u64;
        assert_eq!(sqlite_counter(maximum - 1), maximum - 1);
        assert_eq!(sqlite_counter(maximum), maximum);
        assert_eq!(sqlite_counter(maximum + 1), maximum);
        assert_eq!(sqlite_counter(u64::MAX), maximum);
    }

    #[test]
    fn unsent_samples_coalesce_into_the_latest_cumulative_snapshot() {
        let mut history = SessionHistory::new(7, None);
        history.phase = Phase::Begun;
        history.last_sample_scheduled = Some(SamplePoint {
            at_ms: 0,
            frames_sent: 0,
            bytes_sent: 0,
            dropped_frames: 0,
        });
        history.observe_media(4, 1_000, 1);
        history.sample_if_due(1_000);
        assert_eq!(history.updates.len(), 1);

        history.observe_media(6, 2_000, 2);
        history.sample_if_due(2_000);

        assert_eq!(history.updates.len(), 1);
        let sample = &history.updates[0].request.payload["sample"];
        assert_eq!(sample["frames_sent"], 10);
        assert_eq!(sample["bytes_sent"], 3_000);
        assert_eq!(sample["dropped_frames"], 3);
        assert_eq!(sample["actual_fps"], 5.0);
        assert_eq!(sample["actual_bitrate_kbps"], 12.0);
    }

    #[test]
    fn queued_history_updates_never_exceed_the_session_bound() {
        let mut history = SessionHistory::new(9, None);
        for index in 0..MAX_QUEUED_UPDATES + 4 {
            history.queue_event("test", json!({"index":index}));
        }

        assert_eq!(history.updates.len(), MAX_QUEUED_UPDATES);
    }

    #[test]
    fn zero_duration_final_sample_is_numeric_and_explicitly_limited() {
        let mut history = SessionHistory::new(11, None);
        history.activated(42);
        history.observe_media(1, 321, 0);

        history.teardown_confirmed(42, "stopped", None);

        let sample = history
            .updates
            .iter()
            .find(|update| update.request.payload["action"] == "record_sample")
            .expect("final sample captures short stream totals");
        assert_eq!(sample.request.payload["sample"]["frames_sent"], 1);
        assert_eq!(sample.request.payload["sample"]["bytes_sent"], 321);
        assert_eq!(sample.request.payload["sample"]["actual_fps"], 0.0);
        assert_eq!(sample.request.payload["sample"]["actual_bitrate_kbps"], 0.0);
        assert!(
            sample.request.payload["sample"]["actual_fps"]
                .as_f64()
                .unwrap()
                .is_finite()
        );
        assert!(
            sample.request.payload["sample"]["actual_bitrate_kbps"]
                .as_f64()
                .unwrap()
                .is_finite()
        );
        assert!(
            history.updates[0].request.payload["event"]["payload"]["zero_duration_rates"]
                .as_str()
                .unwrap()
                .contains("numeric zero")
        );
    }
}
