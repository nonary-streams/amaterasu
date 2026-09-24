use super::{
    AudioSinkCaptureOnlyOverrides, CLEANUP_DEADLINE, Controller, Send, app_id,
    call::{Call, Outcome},
    capture_sink_without_routing, key_hex, private_error,
    record::{Record, Stage, State},
    success_reply,
};
use crate::wire::Reply;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Instant;

const LAUNCH_SERVICE: &str = "amaterasu.launch/1";
const RTSP_SERVICE: &str = "amaterasu.rtsp/1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    LaunchApplication,
    LookupApplication,
    OpenRtsp,
    LookupRtsp,
    MonitorApplication,
    MonitorRtsp,
    StopRtsp,
    VerifyRtspStopped,
    StopApplication,
    VerifyApplicationStopped,
}

pub struct Job {
    pub record: u64,
    pub phase: Phase,
    pub call: Call,
    pub deadline: Instant,
}

impl Controller {
    pub(super) fn schedule(&mut self, now: Instant) {
        let Some(id) = self
            .records
            .values()
            .find(|record| needs_work(record, now))
            .map(|record| record.id)
        else {
            return;
        };
        let Some(mut record) = self.records.remove(&id) else {
            return;
        };
        if record.retrying_uncertainty {
            if let Some((phase, mut call)) = record.uncertain.pop_front() {
                call.retry_settlement();
                self.job = Some(Job {
                    record: id,
                    phase,
                    call,
                    deadline: record.deadline,
                });
                self.records.insert(id, record);
                return;
            }
            record.retrying_uncertainty = false;
        }
        let phase = next_phase(&record);
        let call = self.make_call(&mut record, phase);
        let deadline = if matches!(phase, Phase::MonitorApplication | Phase::MonitorRtsp) {
            now + std::time::Duration::from_secs(5)
        } else {
            record.deadline
        };
        self.job = Some(Job {
            record: id,
            phase,
            call,
            deadline,
        });
        self.records.insert(id, record);
    }

    pub(super) fn advance(
        &mut self,
        job: &mut Job,
        now: Instant,
        send: &mut Send<'_>,
        replies: &mut Vec<(u64, Reply)>,
    ) {
        if now >= job.deadline {
            job.call.cancel();
        }
        match job.call.poll(send) {
            Outcome::Waiting => self.job = Some(take_job(job)),
            Outcome::Result(payload) => self.consume(job, payload, now, replies),
            Outcome::Rejected => self.rejected(job, now, replies),
            Outcome::Settled => self.settled(job, now),
            Outcome::Uncertain => {
                let record = self.records.get_mut(&job.record).expect("workflow record");
                let entering_cleanup =
                    !matches!(record.state, State::Stopping | State::CleanupFailed);
                record.uncertain.push_back((job.phase, take_call(job)));
                record.retrying_uncertainty = false;
                record.cleanup_error = Some("broker_settlement_unconfirmed");
                match job.phase {
                    Phase::OpenRtsp | Phase::StopRtsp | Phase::LookupRtsp => {
                        record.rtsp_cleanup_blocked = true;
                    }
                    Phase::LaunchApplication
                    | Phase::StopApplication
                    | Phase::LookupApplication => {
                        record.application_cleanup_blocked = true;
                    }
                    Phase::MonitorApplication | Phase::MonitorRtsp => {}
                    Phase::VerifyRtspStopped => record.rtsp_cleanup_blocked = true,
                    Phase::VerifyApplicationStopped => {
                        record.application_cleanup_blocked = true;
                    }
                }
                record.state = State::Stopping;
                record.stage = Stage::Cleanup;
                if entering_cleanup {
                    record.deadline = now + CLEANUP_DEADLINE;
                }
                self.finish_or_retain(job.record, now, replies);
            }
        }
    }

    fn make_call(&mut self, record: &mut Record, phase: Phase) -> Call {
        if record.rtsp_reconcile == Some(phase) {
            record.rtsp_reconcile = None;
        }
        if record.application_reconcile == Some(phase) {
            record.application_reconcile = None;
        }
        let (service, payload, timeout_ms) = match phase {
            Phase::LaunchApplication => {
                record.application_requested = true;
                (
                    LAUNCH_SERVICE,
                    json!({
                        "action": "launch_export",
                        "operation_id": record.application_operation,
                        "app_id": app_id(record.request.app_id),
                    }),
                    30_000,
                )
            }
            Phase::LookupApplication => (
                LAUNCH_SERVICE,
                json!({
                    "action": "lookup",
                    "operation_id": record.application_operation,
                }),
                5_000,
            ),
            Phase::OpenRtsp => {
                record.rtsp_requested = true;
                let config = self.config.as_ref().expect("enabled streaming config");
                let capture = json!({
                    "capture_provider":record.capture_provider,
                    "constant_rate":record.constant_rate,
                    "output":record.capture_output,
                    "encoder":config.capture.encoder,
                    "audio_endpoint":record.audio_endpoint,
                });
                (
                    RTSP_SERVICE,
                    json!({
                        "action": "open",
                        "operation_id": record.rtsp_operation,
                        "peer_ip": record.peer.remote,
                        "device_name": record.device_name.as_str(),
                        "discover_peers": true,
                        "key_hex": key_hex(&record.request.remote_input_key),
                        "key_id": record.request.key_id,
                        "connect_data": record.connect_data,
                        "stream_audio": record.effective_stream_audio,
                        "capture_sink_without_routing": record.capture_sink_without_routing,
                        "back_button_timeout": record.scoped_back_button_timeout,
                        "ping_timeout_ms": record.global_ping_timeout_ms,
                        "minimum_fps_target": record.global_minimum_fps_target,
                        "wgc_pacing_smoothing": record.global_wgc_pacing_smoothing,
                        "capture": capture,
                        "profile": {
                            "width": record.request.width,
                            "height": record.request.height,
                            "fps": record.request.fps,
                            "bitrate_kbps": record
                                .global_max_bitrate_kbps
                                .map_or(config.profile.bitrate_kbps, |limit| {
                                    config.profile.bitrate_kbps.min(limit)
                                }),
                            "max_reference_frames": config.profile.max_reference_frames,
                            "packet_size": config.profile.packet_size,
                            "fec_percentage": record.effective_fec_percentage,
                            "video_max_batch_size_kb": record.global_video_max_batch_size_kb,
                            "min_parity_shards": config.profile.min_parity_shards,
                            "max_fec_blocks": config.profile.max_fec_blocks,
                            "packet_ms": config.profile.packet_ms,
                            "audio_bitrate": config.profile.audio_bitrate,
                            "encrypt_video": config.profile.encrypt_video,
                            "encrypt_audio": config.profile.encrypt_audio,
                            "permit_legacy_audio_iv_wrap": config.profile.permit_legacy_audio_iv_wrap,
                        },
                    }),
                    15_000,
                )
            }
            Phase::LookupRtsp => (
                RTSP_SERVICE,
                json!({"action":"lookup","operation_id":record.rtsp_operation}),
                5_000,
            ),
            Phase::MonitorApplication | Phase::VerifyApplicationStopped => (
                LAUNCH_SERVICE,
                json!({"action":"status","launch_id":record.application_launch}),
                5_000,
            ),
            Phase::MonitorRtsp | Phase::VerifyRtspStopped => (
                RTSP_SERVICE,
                json!({"action":"status","ticket":record.rtsp_ticket}),
                5_000,
            ),
            Phase::StopRtsp => (
                RTSP_SERVICE,
                json!({"action":"close","ticket":record.rtsp_ticket}),
                15_000,
            ),
            Phase::StopApplication => (
                LAUNCH_SERVICE,
                json!({"action":"stop","launch_id":record.application_launch}),
                15_000,
            ),
        };
        let operation = self.next_operation(record, phase.name());
        Call::new(service, payload, operation, timeout_ms)
    }

    fn consume(
        &mut self,
        job: &Job,
        payload: Value,
        now: Instant,
        replies: &mut Vec<(u64, Reply)>,
    ) {
        let record = self.records.get_mut(&job.record).expect("workflow record");
        match job.phase {
            Phase::LaunchApplication | Phase::LookupApplication => {
                match application_response(record, &payload) {
                    Ok((launch_id, "active", true)) if record.state == State::Starting => {
                        record.application_launch = Some(launch_id);
                        match application_config_overrides(&payload) {
                            Ok(application_overrides) => {
                                let application_fec_percentage =
                                    application_overrides.fec_percentage;
                                record.application_fec_percentage = application_fec_percentage;
                                record.effective_fec_percentage = record
                                    .peer
                                    .client_fec_percentage
                                    .or(application_fec_percentage)
                                    .unwrap_or(record.global_fec_percentage);
                                record.application_stream_audio =
                                    application_overrides.stream_audio;
                                record.effective_stream_audio = record
                                    .peer
                                    .client_stream_audio
                                    .or(application_overrides.stream_audio)
                                    .unwrap_or(record.global_stream_audio);
                                let scoped_audio_sink = record
                                    .peer
                                    .client_audio_sink
                                    .as_deref()
                                    .or(application_overrides.audio_sink.as_deref());
                                let resolved_configured_audio_sink = match scoped_audio_sink {
                                    Some("") => None,
                                    Some(audio_sink) => Some(audio_sink.to_owned()),
                                    None => record.global_configured_audio_sink.clone(),
                                };
                                record.audio_endpoint = resolved_configured_audio_sink
                                    .as_deref()
                                    .unwrap_or(&record.bootstrap_audio_endpoint)
                                    .to_owned();
                                // The endpoint has now been resolved using global, application,
                                // then client precedence. Amaterasu has no virtual-sink setting.
                                record.capture_sink_without_routing = capture_sink_without_routing(
                                    record.global_audio_sink_capture_only,
                                    AudioSinkCaptureOnlyOverrides {
                                        application: application_overrides.audio_sink_capture_only,
                                        client: record.peer.client_audio_sink_capture_only,
                                    },
                                    resolved_configured_audio_sink.as_deref(),
                                    &record.audio_endpoint,
                                    None,
                                );
                                record.scoped_back_button_timeout = record
                                    .peer
                                    .client_back_button_timeout
                                    .or(application_overrides.back_button_timeout);
                                let mut profile = self
                                    .config
                                    .as_ref()
                                    .expect("enabled streaming config")
                                    .profile
                                    .clone();
                                profile.fec_percentage = record.effective_fec_percentage;
                                if profile.validate().is_ok() {
                                    record.stage = Stage::OpenRtsp;
                                } else {
                                    fail_record(record, "invalid_streaming_profile_override", now);
                                }
                            }
                            Err(code) => fail_record(record, code, now),
                        }
                    }
                    Ok((launch_id, "active", _)) => {
                        record.application_launch = Some(launch_id);
                        record.request_cleanup(
                            Some("application_launch_unhealthy"),
                            now + CLEANUP_DEADLINE,
                        );
                    }
                    Ok((launch_id, _, _)) => {
                        record.application_launch = Some(launch_id);
                        record.failure.get_or_insert("application_ended");
                        record.request_cleanup(None, now + CLEANUP_DEADLINE);
                    }
                    Err("launch_not_found") if job.phase == Phase::LookupApplication => {
                        record.application_requested = false;
                        record
                            .failure
                            .get_or_insert("application_launch_unavailable");
                        record.request_cleanup(None, now + CLEANUP_DEADLINE);
                    }
                    Err(code) => fail_record(record, code, now),
                }
            }
            Phase::OpenRtsp | Phase::LookupRtsp => match rtsp_response(record, &payload) {
                Ok((ticket, url, "ready", true)) if record.state == State::Starting => {
                    record.rtsp_ticket = Some(ticket);
                    record.rtsp_url = Some(url.to_owned());
                    record.state = State::AwaitingDelivery;
                    record.stage = Stage::AwaitingDelivery;
                    if let Some(connection) = record.launch_connection {
                        replies.push((connection, success_reply(record)));
                    } else {
                        record.request_cleanup(
                            Some("launch_response_unavailable"),
                            now + CLEANUP_DEADLINE,
                        );
                    }
                }
                Ok((ticket, _, _, _)) => {
                    record.rtsp_ticket = Some(ticket);
                    record.request_cleanup(None, now + CLEANUP_DEADLINE);
                }
                Err("ticket_not_found") if job.phase == Phase::LookupRtsp => {
                    record.rtsp_requested = false;
                    fail_record(record, "rtsp_launch_unavailable", now);
                }
                Err(code) => fail_record(record, code, now),
            },
            Phase::MonitorApplication => match application_response(record, &payload) {
                Ok((_, "active", true)) => record.monitored(now),
                Ok(_) => record.request_cleanup(Some("application_ended"), now + CLEANUP_DEADLINE),
                Err(code) => fail_record(record, code, now),
            },
            Phase::MonitorRtsp => match rtsp_response(record, &payload) {
                Ok((_, _, "ready" | "arming" | "playing", true)) => record.monitored(now),
                Ok(_) => record.request_cleanup(Some("stream_ended"), now + CLEANUP_DEADLINE),
                Err(code) => fail_record(record, code, now),
            },
            Phase::StopRtsp | Phase::VerifyRtspStopped => {
                if clean_rtsp_terminal(record, &payload) {
                    record.rtsp_requested = false;
                    record.rtsp_ticket = None;
                    record.rtsp_url = None;
                } else if job.phase == Phase::StopRtsp {
                    record.stage = Stage::VerifyRtspCleanup;
                } else {
                    record.cleanup_error = Some("rtsp_cleanup_unconfirmed");
                    record.rtsp_cleanup_blocked = true;
                    record.stage = Stage::Cleanup;
                }
            }
            Phase::StopApplication | Phase::VerifyApplicationStopped => {
                if clean_application_terminal(record, &payload) {
                    record.application_requested = false;
                    record.application_launch = None;
                } else if job.phase == Phase::StopApplication {
                    record.stage = Stage::VerifyApplicationCleanup;
                } else {
                    record.cleanup_error = Some("application_cleanup_unconfirmed");
                    record.application_cleanup_blocked = true;
                    record.stage = Stage::Cleanup;
                }
            }
        }
        self.finish_or_retain(job.record, now, replies);
    }

    fn rejected(&mut self, job: &Job, now: Instant, replies: &mut Vec<(u64, Reply)>) {
        let record = self.records.get_mut(&job.record).expect("workflow record");
        match job.phase {
            Phase::LookupApplication => {
                record.fail_cleanup("application_lookup_unavailable");
            }
            Phase::LookupRtsp => {
                record.fail_cleanup("rtsp_lookup_unavailable");
            }
            Phase::StopRtsp | Phase::VerifyRtspStopped => {
                record.cleanup_error = Some("rtsp_cleanup_failed");
                record.rtsp_cleanup_blocked = true;
                record.stage = Stage::Cleanup;
            }
            Phase::StopApplication | Phase::VerifyApplicationStopped => {
                record.cleanup_error = Some("application_cleanup_failed");
                record.application_cleanup_blocked = true;
                record.stage = Stage::Cleanup;
            }
            Phase::MonitorApplication | Phase::MonitorRtsp => {
                fail_record(record, "lifecycle_status_unavailable", now)
            }
            Phase::LaunchApplication => {
                record.application_requested = false;
                fail_record(record, "application_launch_rejected", now);
            }
            Phase::OpenRtsp => {
                record.rtsp_requested = false;
                fail_record(record, "rtsp_launch_rejected", now);
            }
        }
        self.finish_or_retain(job.record, now, replies);
    }

    fn settled(&mut self, job: &Job, now: Instant) {
        let record = self.records.get_mut(&job.record).expect("workflow record");
        record.stage = Stage::Cleanup;
        match job.phase {
            Phase::LaunchApplication => {
                record.application_cleanup_blocked = false;
                record.application_reconcile = Some(Phase::LookupApplication);
            }
            Phase::OpenRtsp => {
                record.rtsp_cleanup_blocked = false;
                record.rtsp_reconcile = Some(Phase::LookupRtsp);
            }
            Phase::StopRtsp => {
                record.rtsp_cleanup_blocked = false;
                record.rtsp_reconcile = Some(Phase::VerifyRtspStopped);
            }
            Phase::StopApplication => {
                record.application_cleanup_blocked = false;
                record.application_reconcile = Some(Phase::VerifyApplicationStopped);
            }
            Phase::LookupApplication | Phase::VerifyApplicationStopped => {
                record.application_cleanup_blocked = false;
                record.application_reconcile = Some(job.phase);
            }
            Phase::LookupRtsp | Phase::VerifyRtspStopped => {
                record.rtsp_cleanup_blocked = false;
                record.rtsp_reconcile = Some(job.phase);
            }
            Phase::MonitorApplication | Phase::MonitorRtsp => {
                record
                    .request_cleanup(Some("lifecycle_status_unavailable"), now + CLEANUP_DEADLINE);
            }
        }
        if record.state == State::Starting && now >= record.deadline {
            record.request_cleanup(Some("launch_deadline_exceeded"), now + CLEANUP_DEADLINE);
        }
    }

    fn finish_or_retain(&mut self, record_id: u64, now: Instant, replies: &mut Vec<(u64, Reply)>) {
        let record = self.records.get_mut(&record_id).expect("workflow record");
        if record.state == State::Stopping {
            if now >= record.deadline && (record.rtsp_requested || record.application_requested) {
                record.fail_cleanup("cleanup_deadline_exceeded");
                reply_failure(record, replies);
            } else if !record.rtsp_requested
                && !record.application_requested
                && record.uncertain.is_empty()
                && record.rtsp_reconcile.is_none()
                && record.application_reconcile.is_none()
            {
                record.finish_cleanup();
                if let Some(connection) = record.cancel_connection {
                    replies.push((connection, super::cancel_reply()));
                }
                record.cancel_connection = None;
                reply_failure(record, replies);
            } else if !record.retrying_uncertainty
                && (!record.rtsp_requested || record.rtsp_cleanup_blocked)
                && (!record.application_requested || record.application_cleanup_blocked)
            {
                record.state = State::CleanupFailed;
                reply_failure(record, replies);
            }
        }
    }
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::LaunchApplication => "launch",
            Self::LookupApplication => "find-launch",
            Self::OpenRtsp => "open-rtsp",
            Self::LookupRtsp => "find-rtsp",
            Self::MonitorApplication => "application-status",
            Self::MonitorRtsp => "rtsp-status",
            Self::StopRtsp => "stop-rtsp",
            Self::VerifyRtspStopped => "verify-rtsp-stop",
            Self::StopApplication => "stop-application",
            Self::VerifyApplicationStopped => "verify-application-stop",
        }
    }
}

fn needs_work(record: &Record, now: Instant) -> bool {
    if record.state == State::CleanupFailed {
        return false;
    }
    if record.retrying_uncertainty {
        return true;
    }
    matches!(record.stage, Stage::LaunchApplication | Stage::OpenRtsp)
        || (record.stage == Stage::Live && now >= record.next_monitor)
        || matches!(
            record.stage,
            Stage::Cleanup | Stage::VerifyRtspCleanup | Stage::VerifyApplicationCleanup
        )
}

fn next_phase(record: &Record) -> Phase {
    if let Some(phase) = record.rtsp_reconcile {
        return phase;
    }
    if let Some(phase) = record.application_reconcile {
        return phase;
    }
    match record.stage {
        Stage::LaunchApplication => Phase::LaunchApplication,
        Stage::OpenRtsp => Phase::OpenRtsp,
        Stage::Live if record.monitor_application_next => Phase::MonitorApplication,
        Stage::Live => Phase::MonitorRtsp,
        Stage::Cleanup
            if record.rtsp_requested
                && !record.rtsp_cleanup_blocked
                && record.rtsp_ticket.is_none() =>
        {
            Phase::LookupRtsp
        }
        Stage::Cleanup if !record.rtsp_cleanup_blocked && record.rtsp_ticket.is_some() => {
            Phase::StopRtsp
        }
        Stage::Cleanup
            if record.application_requested
                && !record.application_cleanup_blocked
                && record.application_launch.is_none() =>
        {
            Phase::LookupApplication
        }
        Stage::Cleanup
            if !record.application_cleanup_blocked && record.application_launch.is_some() =>
        {
            Phase::StopApplication
        }
        Stage::VerifyRtspCleanup => Phase::VerifyRtspStopped,
        Stage::VerifyApplicationCleanup => Phase::VerifyApplicationStopped,
        _ => unreachable!("scheduled record has work"),
    }
}

fn fail_record(record: &mut Record, code: &'static str, now: Instant) {
    record.request_cleanup(Some(code), now + CLEANUP_DEADLINE);
}

fn application_response<'a>(
    record: &Record,
    payload: &'a Value,
) -> Result<(u64, &'a str, bool), &'static str> {
    if payload["ok"] != true {
        return Err(match payload["code"].as_str() {
            Some("launch_not_found") => "launch_not_found",
            _ => "invalid_application_response",
        });
    }
    let launch = payload
        .get("launch")
        .filter(|value| value.is_object())
        .ok_or("invalid_application_response")?;
    let id = launch["launch_id"]
        .as_u64()
        .filter(|id| *id != 0)
        .ok_or("invalid_application_response")?;
    if record
        .application_launch
        .is_some_and(|expected| expected != id)
        || launch.get("failure").is_none()
        || launch.get("cleanup_error").is_none()
    {
        return Err("invalid_application_response");
    }
    if launch["operation_id"] != record.application_operation
        || launch["app_id"] != app_id(record.request.app_id)
    {
        return Err("invalid_application_response");
    }
    let state = launch["state"]
        .as_str()
        .filter(|state| {
            matches!(
                *state,
                "starting"
                    | "active"
                    | "stopping"
                    | "stopped"
                    | "cancelled"
                    | "failed"
                    | "cleanup_failed"
            )
        })
        .ok_or("invalid_application_response")?;
    Ok((id, state, launch["cleanup_error"].is_null()))
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ApplicationConfigOverrides {
    fec_percentage: Option<u8>,
    stream_audio: Option<bool>,
    audio_sink: Option<String>,
    audio_sink_capture_only: Option<bool>,
    back_button_timeout: Option<i32>,
}

fn application_config_overrides(
    payload: &Value,
) -> Result<ApplicationConfigOverrides, &'static str> {
    let launch = payload
        .get("launch")
        .filter(|value| value.is_object())
        .ok_or("invalid_application_response")?;
    let config_overrides = match launch.get("config_overrides") {
        None | Some(Value::Null) => ApplicationConfigOverrides::default(),
        Some(value) => {
            serde_json::from_value(value.clone()).map_err(|_| "invalid_application_response")?
        }
    };
    if config_overrides.fec_percentage == Some(0)
        || config_overrides
            .audio_sink
            .as_deref()
            .is_some_and(|value| !super::valid_audio_sink_override(value))
    {
        return Err("invalid_application_response");
    }
    Ok(config_overrides)
}

fn rtsp_response<'a>(
    record: &Record,
    payload: &'a Value,
) -> Result<(u64, &'a str, &'a str, bool), &'static str> {
    if payload["ok"] != true {
        return Err(match payload["code"].as_str() {
            Some("ticket_not_found") => "ticket_not_found",
            _ => "invalid_rtsp_response",
        });
    }
    let ticket = payload["ticket"]
        .as_u64()
        .filter(|ticket| *ticket != 0)
        .ok_or("invalid_rtsp_response")?;
    if record
        .rtsp_ticket
        .is_some_and(|expected| expected != ticket)
        || payload.get("failure").is_none()
        || payload.get("cleanup_error").is_none()
    {
        return Err("invalid_rtsp_response");
    }
    let state = payload["state"]
        .as_str()
        .filter(|state| {
            matches!(
                *state,
                "preparing"
                    | "ready"
                    | "arming"
                    | "playing"
                    | "closing"
                    | "closed"
                    | "failed"
                    | "cleanup_failed"
            )
        })
        .ok_or("invalid_rtsp_response")?;
    let url = payload["rtsp_url"]
        .as_str()
        .filter(|url| {
            url.starts_with("rtspenc://") && url.len() <= 256 && !url.chars().any(char::is_control)
        })
        .ok_or("invalid_rtsp_response")?;
    Ok((ticket, url, state, payload["cleanup_error"].is_null()))
}

fn clean_rtsp_terminal(record: &Record, payload: &Value) -> bool {
    rtsp_response(record, payload)
        .is_ok_and(|(_, _, state, healthy)| healthy && matches!(state, "closed" | "failed"))
        && payload["cleanup_error"].is_null()
}

fn clean_application_terminal(record: &Record, payload: &Value) -> bool {
    application_response(record, payload).is_ok_and(|(_, state, healthy)| {
        healthy && matches!(state, "stopped" | "cancelled" | "failed")
    }) && payload["cleanup_error"].is_null()
}

fn take_job(job: &mut Job) -> Job {
    // Replacing is needed only to move the retained exact Call without cloning it.
    let replacement = Call::new("invalid", json!({}), "invalid".into(), 1);
    Job {
        record: job.record,
        phase: job.phase,
        call: std::mem::replace(&mut job.call, replacement),
        deadline: job.deadline,
    }
}

fn take_call(job: &mut Job) -> Call {
    let replacement = Call::new("invalid", json!({}), "invalid".into(), 1);
    std::mem::replace(&mut job.call, replacement)
}

fn reply_failure(record: &mut Record, replies: &mut Vec<(u64, Reply)>) {
    if let Some(connection) = record.launch_connection.take() {
        replies.push((connection, private_error(503, "stream launch failed")));
    }
    if record.state == State::CleanupFailed
        && let Some(connection) = record.cancel_connection.take()
    {
        replies.push((
            connection,
            private_error(503, "stream cleanup is incomplete"),
        ));
    }
}
