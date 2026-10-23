use super::{
    AudioSinkCaptureOnlyOverrides, CLEANUP_DEADLINE, Controller, Send, app_id,
    call::{Call, Outcome},
    capture_sink_without_routing, key_hex, private_error,
    record::{Record, Stage, State},
    success_reply,
};
use crate::frame_limiter::FrameLimiterSettings;
use crate::wire::Reply;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Instant;

const LAUNCH_SERVICE: &str = "amaterasu.launch/1";
const RTSP_SERVICE: &str = "amaterasu.rtsp/1";
const DISPLAY_SERVICE: &str = "amaterasu.display/1";
const STREAM_SESSION_SERVICE: &str = "amaterasu.stream-session/1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    ReserveHistory,
    PreflightApplication,
    SnapshotDisplay,
    EnumerateDisplay,
    ConfirmDisplay,
    LaunchApplication,
    LookupApplication,
    PrepareDisplayAdmission,
    OpenRtsp,
    LookupRtsp,
    MonitorApplication,
    MonitorRtsp,
    StopRtsp,
    VerifyRtspStopped,
    StopApplication,
    VerifyApplicationStopped,
    ReleaseHistory,
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
            Outcome::Settled => self.settled(job, now, replies),
            Outcome::Uncertain => {
                let record = self.records.get_mut(&job.record).expect("workflow record");
                let entering_cleanup =
                    !matches!(record.state, State::Stopping | State::CleanupFailed);
                record.uncertain.push_back((job.phase, take_call(job)));
                record.retrying_uncertainty = false;
                record.cleanup_error = Some("broker_settlement_unconfirmed");
                match job.phase {
                    Phase::ReserveHistory | Phase::ReleaseHistory => {}
                    Phase::OpenRtsp | Phase::StopRtsp | Phase::LookupRtsp => {
                        record.rtsp_cleanup_blocked = true;
                    }
                    Phase::LaunchApplication
                    | Phase::PrepareDisplayAdmission
                    | Phase::StopApplication
                    | Phase::LookupApplication => {
                        record.application_cleanup_blocked = true;
                    }
                    Phase::PreflightApplication
                    | Phase::SnapshotDisplay
                    | Phase::EnumerateDisplay
                    | Phase::ConfirmDisplay => {}
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

    fn advance_after_history(&self, record: &mut Record, now: Instant) {
        if record.state != State::Starting {
            return;
        }
        if !record.desktop {
            record.stage = Stage::PreflightApplication;
            return;
        }
        let Some(config) = &self.config else {
            fail_record(record, "streaming_disabled", now);
            return;
        };
        let mut profile = config.profile.clone();
        if let Err(code) = project_application_policy(
            record,
            &ApplicationConfigOverrides::default(),
            &mut profile,
        ) {
            fail_record(record, code, now);
            return;
        }
        record.application_display_overrides = Some(json!({}));
        record.display_policy_snapshot = Some(display_policy_snapshot(record));
        record.frame_limiter_snapshot = Some(frame_limiter_launch_snapshot(record));
        record.stage = Stage::OpenRtsp;
    }

    fn make_call(&mut self, record: &mut Record, phase: Phase) -> Call {
        if record.rtsp_reconcile == Some(phase) {
            record.rtsp_reconcile = None;
        }
        if record.application_reconcile == Some(phase) {
            record.application_reconcile = None;
        }
        let (service, mut payload, timeout_ms) = match phase {
            Phase::ReserveHistory => {
                record.history_release_required = true;
                (
                    STREAM_SESSION_SERVICE,
                    json!({"action":"reserve_history_activity", "activity_id":record.rtsp_operation,
                        "rtsp_operation_id":record.rtsp_operation}),
                    5_000,
                )
            }
            Phase::PreflightApplication => (
                LAUNCH_SERVICE,
                json!({
                    "action": "preflight_export",
                    "operation_id": record.application_operation,
                    "app_id": app_id(record.request.app_id),
                    "global_preparation": record.global_preparation,
                    "legacy_global_preparation": record.legacy_global_preparation,
                }),
                10_000,
            ),
            Phase::SnapshotDisplay | Phase::ConfirmDisplay => {
                (DISPLAY_SERVICE, json!({"action":"snapshot"}), 5_000)
            }
            Phase::EnumerateDisplay => (
                DISPLAY_SERVICE,
                json!({
                    "action":"enumerate",
                    "offset":record.display_enumeration_offset,
                    "limit":32,
                }),
                5_000,
            ),
            Phase::LaunchApplication => {
                record.application_requested = true;
                (
                    LAUNCH_SERVICE,
                    json!({
                        "action": "launch_export",
                        "operation_id": record.application_operation,
                        "app_id": app_id(record.request.app_id),
                        "global_preparation": record.global_preparation,
                        "legacy_global_preparation": record.legacy_global_preparation,
                        "preflight_token": record.application_preflight_token,
                        "display_admission_token": record.display_admission_token,
                        "application_display_overrides": record.application_display_overrides,
                        "frame_limiter_settings": record.frame_limiter_snapshot,
                        "client_frame_limiter_overrides":
                            client_frame_limiter_overrides(record),
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
            Phase::PrepareDisplayAdmission => {
                let snapshot = record
                    .display_policy_snapshot
                    .as_ref()
                    .expect("frozen display policy");
                let request = record
                    .display_request
                    .as_ref()
                    .expect("complete display request");
                let request_sha256 = request_sha256(request).expect("serialized display request");
                (
                    LAUNCH_SERVICE,
                    json!({
                        "action":"prepare_display_admission",
                        "application_operation_id":record.application_operation,
                        "global_revision":snapshot["global_revision"],
                        "global_overrides":snapshot["global_overrides"],
                        "catalog_revision":snapshot["catalog_revision"],
                        "application_display_overrides":snapshot["application_overrides"],
                        "certificate_id":snapshot["certificate_id"],
                        "client_display_overrides":snapshot["client_overrides"],
                        "rtsp_operation_id":record.rtsp_operation,
                        "request_sha256":request_sha256,
                    }),
                    5_000,
                )
            }
            Phase::OpenRtsp => {
                record.rtsp_requested = true;
                let config = self.config.as_ref().expect("enabled streaming config");
                let capture = json!({
                    "capture_provider":record.capture_provider,
                    "constant_rate":record.constant_rate,
                    "output":record.capture_output,
                    "encoder":config.capture.encoder,
                    "audio_endpoint":record.audio_endpoint,
                    "host_audio_enabled":record.request.host_audio_enabled,
                });
                let profile = json!({
                    "width": record.request.width,
                    "height": record.request.height,
                    "fps": record.request.fps,
                    "bitrate_kbps": config.profile.bitrate_kbps,
                    "max_reference_frames": config.profile.max_reference_frames,
                    "packet_size": config.profile.packet_size,
                    "fec_percentage": record.effective_fec_percentage,
                    "video_max_batch_size_kb": record.effective_video_max_batch_size_kb,
                    "min_parity_shards": config.profile.min_parity_shards,
                    "max_fec_blocks": config.profile.max_fec_blocks,
                    "packet_ms": config.profile.packet_ms,
                    "audio_bitrate": config.profile.audio_bitrate,
                    "encrypt_video": config.profile.encrypt_video,
                    "encrypt_audio": config.profile.encrypt_audio,
                    "permit_legacy_audio_iv_wrap": config.profile.permit_legacy_audio_iv_wrap,
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
                        "continuous_audio":
                            record.request.continuous_audio && record.effective_stream_audio,
                        "capture_sink_without_routing": record.capture_sink_without_routing,
                        "virtual_sink": record.resolved_virtual_sink,
                        "channels": record.resolved_virtual_sink.as_ref().map(|_| record.request.audio_channels),
                        "back_button_timeout": record.scoped_back_button_timeout,
                        "controller": record.scoped_controller,
                        "gamepad": record.scoped_gamepad,
                        "keyboard": record.scoped_keyboard,
                        "mouse": record.scoped_mouse,
                        "native_pen_touch": record.scoped_native_pen_touch,
                        "ds4_back_as_touchpad_click": record
                            .scoped_ds4_back_as_touchpad_click,
                        "high_resolution_scrolling": record.scoped_high_resolution_scrolling,
                        "always_send_scancodes": record.scoped_always_send_scancodes,
                        "key_rightalt_to_key_win": record.scoped_key_rightalt_to_key_win,
                        "key_repeat_delay": record.scoped_key_repeat_delay,
                        "key_repeat_frequency": record.scoped_key_repeat_frequency,
                        "ping_timeout_ms": record.global_ping_timeout_ms,
                        "max_bitrate_kbps": record.effective_max_bitrate_kbps,
                        "minimum_fps_target": record.effective_minimum_fps_target,
                        "wgc_pacing_smoothing": record.global_wgc_pacing_smoothing,
                        "frame_limiter_settings": record.executable_path.as_ref().and(record.frame_limiter_snapshot.as_ref()),
                        "frame_limiter_executable_path": record.executable_path,
                        "display_policy_snapshot": record.display_policy_snapshot,
                        "display_request": record.display_request,
                        "capture": capture,
                        "profile": profile,
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
            Phase::ReleaseHistory => (
                STREAM_SESSION_SERVICE,
                json!({"action":"release_history_activity", "activity_id":record.rtsp_operation,
                    "rtsp_operation_id":record.rtsp_operation}),
                5_000,
            ),
        };
        if matches!(phase, Phase::OpenRtsp) && record.history_reserved {
            payload["history_activity_id"] = json!(record.rtsp_operation);
        }
        if matches!(phase, Phase::OpenRtsp)
            && let Some(policy) = record.video_encryption_policy
        {
            payload["video_encryption_policy"] = json!({
                "lan_mode": policy.lan_mode,
                "wan_mode": policy.wan_mode,
            });
        }
        if matches!(phase, Phase::OpenRtsp) && record.display_request.is_none() {
            payload
                .as_object_mut()
                .expect("RTSP open payload")
                .remove("display_request");
        }
        if matches!(phase, Phase::OpenRtsp) && record.display_request.is_some() {
            payload["display_admission_token"] = json!(record.display_admission_token);
        }
        if matches!(phase, Phase::OpenRtsp)
            && (record.display_request.is_some()
                || (record.executable_path.is_some() && record.frame_limiter_snapshot.is_some()))
        {
            payload["application_operation_id"] = json!(record.application_operation);
            payload["app_id"] = json!(app_id(record.request.app_id));
            payload["catalog_revision"] = json!(
                record
                    .application_catalog_revision
                    .expect("RTSP admission follows application preflight")
            );
        }
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
            Phase::ReserveHistory => {
                if record.cleanup_error == Some("broker_settlement_unconfirmed") {
                    record.cleanup_error = None;
                }
                record.history_reserved = payload["ok"] == true
                    && payload["reserved"] == true
                    && payload["activity_id"] == record.rtsp_operation
                    && payload["rtsp_operation_id"] == record.rtsp_operation;
                let declined = payload["ok"] == true
                    && payload["reserved"] == false
                    && payload["activity_id"] == record.rtsp_operation
                    && payload["rtsp_operation_id"] == record.rtsp_operation;
                if declined {
                    record.history_release_required = false;
                }
                record.history_state = if record.history_reserved {
                    "reserved"
                } else {
                    "unavailable"
                };
                self.advance_after_history(record, now);
            }
            Phase::PreflightApplication => match application_preflight_response(record, &payload) {
                Ok((token, catalog_revision, application_overrides, executable_path))
                    if record.state == State::Starting =>
                {
                    record.application_preflight_token = Some(token);
                    record.application_catalog_revision = Some(catalog_revision);
                    record.executable_path = executable_path;
                    record.application_frame_limiter_settings =
                        application_overrides.frame_limiter_settings();
                    record.application_preflight_overrides = Some(application_overrides);
                    record.application_display_overrides = Some(application_display_overrides(
                        payload.get("config_overrides"),
                    ));
                    record.display_policy_snapshot = Some(display_policy_snapshot(record));
                    record.frame_limiter_snapshot = Some(frame_limiter_launch_snapshot(record));
                    let frozen_overrides = record
                        .application_preflight_overrides
                        .as_ref()
                        .expect("validated preflight overrides")
                        .clone();
                    let mut profile = self
                        .config
                        .as_ref()
                        .expect("enabled streaming config")
                        .profile
                        .clone();
                    if let Err(code) =
                        project_application_policy(record, &frozen_overrides, &mut profile)
                    {
                        fail_record(record, code, now);
                        self.finish_or_retain(job.record, now, replies);
                        return;
                    }
                    match record
                        .display_policy_snapshot
                        .as_ref()
                        .and_then(|snapshot| {
                            snapshot["effective_overrides"]["dd_configuration_option"].as_str()
                        }) {
                        Some(
                            "verify_only"
                            | "ensure_active"
                            | "ensure_primary"
                            | "ensure_only_display",
                        ) => {
                            if capture_source(record).is_ok() {
                                record.stage = Stage::SnapshotDisplay;
                            } else {
                                fail_record(record, "invalid_capture_output", now);
                            }
                        }
                        None | Some("disabled") => record.stage = Stage::LaunchApplication,
                        Some(_) => fail_record(record, "display_mutation_unavailable", now),
                    }
                }
                Ok(_) => fail_record(record, "application_preflight_unavailable", now),
                Err(code) => fail_record(record, code, now),
            },
            Phase::SnapshotDisplay => match display_snapshot(&payload) {
                Ok((revision, configuration, ids)) if record.state == State::Starting => {
                    record.display_topology_revision = Some(revision);
                    record.display_snapshot_configuration = Some(configuration);
                    record.display_snapshot_ids = ids;
                    record.stage = Stage::EnumerateDisplay;
                }
                Ok(_) => fail_record(record, "display_snapshot_unavailable", now),
                Err(code) => fail_record(record, code, now),
            },
            Phase::EnumerateDisplay if record.state == State::Starting => {
                match display_enumeration(record, &payload) {
                    Ok(Some(next_offset)) => {
                        record.display_enumeration_offset = next_offset;
                    }
                    Ok(None) => record.stage = Stage::ConfirmDisplay,
                    Err(code) => fail_record(record, code, now),
                }
            }
            Phase::EnumerateDisplay => fail_record(record, "display_enumeration_unavailable", now),
            Phase::ConfirmDisplay => match display_snapshot(&payload) {
                Ok((revision, configuration, ids))
                    if record.state == State::Starting
                        && record.display_match_id.is_some()
                        && record.display_topology_revision.as_deref() == Some(&revision)
                        && ids == record.display_snapshot_ids
                        && record.display_snapshot_configuration.as_ref()
                            == Some(&configuration) =>
                {
                    record.verified_display_id = record.display_match_id.clone();
                    match build_display_request(record) {
                        Ok(request) => {
                            record.display_request = request;
                            record.stage = if record.display_request.is_some() {
                                Stage::PrepareDisplayAdmission
                            } else {
                                Stage::LaunchApplication
                            };
                        }
                        Err(code) => fail_record(record, code, now),
                    }
                }
                Ok(_) => fail_record(record, "display_topology_changed", now),
                Err(code) => fail_record(record, code, now),
            },
            Phase::LaunchApplication | Phase::LookupApplication => {
                match application_response(record, &payload) {
                    Ok((launch_id, "active", true)) if record.state == State::Starting => {
                        record.application_launch = Some(launch_id);
                        match application_config_overrides(&payload) {
                            Ok(application_overrides) => {
                                if record.application_preflight_overrides.as_ref()
                                    != Some(&application_overrides)
                                {
                                    fail_record(record, "application_policy_mismatch", now);
                                    self.finish_or_retain(job.record, now, replies);
                                    return;
                                }
                                if payload["launch"]["catalog_revision"].as_u64()
                                    != record.application_catalog_revision
                                {
                                    fail_record(record, "application_revision_mismatch", now);
                                    self.finish_or_retain(job.record, now, replies);
                                    return;
                                }
                                if payload["launch"].get("application_display_overrides")
                                    != record.application_display_overrides.as_ref()
                                {
                                    fail_record(record, "application_display_policy_mismatch", now);
                                    self.finish_or_retain(job.record, now, replies);
                                    return;
                                }
                                record.application_frame_limiter_settings =
                                    application_overrides.frame_limiter_settings();
                                if record.frame_limiter_snapshot.as_ref()
                                    != payload["launch"].get("frame_limiter_settings")
                                {
                                    fail_record(record, "application_snapshot_mismatch", now);
                                    self.finish_or_retain(job.record, now, replies);
                                    return;
                                }
                                if record.executable_path.as_deref()
                                    != payload["launch"]["executable_path"].as_str()
                                {
                                    fail_record(record, "application_target_mismatch", now);
                                    self.finish_or_retain(job.record, now, replies);
                                    return;
                                }
                                if record.display_request.is_some() {
                                    complete_launch(record, now, replies);
                                } else {
                                    record.stage = Stage::OpenRtsp;
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
            Phase::PrepareDisplayAdmission => match display_admission_response(record, &payload) {
                Ok(token) if record.state == State::Starting => {
                    record.display_admission_token = Some(token);
                    record.stage = Stage::OpenRtsp;
                }
                Ok(_) => fail_record(record, "display_admission_unavailable", now),
                Err(code) => fail_record(record, code, now),
            },
            Phase::OpenRtsp | Phase::LookupRtsp => match rtsp_response(record, &payload) {
                Ok((ticket, url, "ready", true)) if record.state == State::Starting => {
                    record.rtsp_ticket = Some(ticket);
                    record.rtsp_url = Some(url.to_owned());
                    if record.display_request.is_some() && record.application_launch.is_none() {
                        record.stage = Stage::LaunchApplication;
                    } else {
                        complete_launch(record, now, replies);
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
            Phase::ReleaseHistory => {
                let confirmed = payload["ok"] == true
                    && payload["released"].is_boolean()
                    && payload["activity_id"] == record.rtsp_operation
                    && payload["rtsp_operation_id"] == record.rtsp_operation;
                if confirmed {
                    record.history_release_required = false;
                    record.history_state = if payload["released"] == true {
                        "released"
                    } else {
                        "unavailable"
                    };
                } else {
                    record.cleanup_error = Some("history_release_unconfirmed");
                    record.stage = Stage::Cleanup;
                }
            }
        }
        self.finish_or_retain(job.record, now, replies);
    }

    fn rejected(&mut self, job: &Job, now: Instant, replies: &mut Vec<(u64, Reply)>) {
        let record = self.records.get_mut(&job.record).expect("workflow record");
        match job.phase {
            Phase::ReserveHistory => {
                if record.cleanup_error == Some("broker_settlement_unconfirmed") {
                    record.cleanup_error = None;
                }
                record.history_state = "unavailable";
                record.history_release_required = false;
                self.advance_after_history(record, now);
            }
            Phase::ReleaseHistory => {
                record.cleanup_error = Some("history_release_unavailable");
                record.stage = Stage::Cleanup;
            }
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
            Phase::PrepareDisplayAdmission => {
                fail_record(record, "display_admission_unavailable", now);
            }
            Phase::PreflightApplication => {
                fail_record(record, "application_preflight_rejected", now)
            }
            Phase::SnapshotDisplay | Phase::EnumerateDisplay | Phase::ConfirmDisplay => {
                fail_record(record, "display_verification_unavailable", now)
            }
            Phase::OpenRtsp => {
                record.rtsp_requested = false;
                fail_record(record, "rtsp_launch_rejected", now);
            }
        }
        self.finish_or_retain(job.record, now, replies);
    }

    fn settled(&mut self, job: &Job, now: Instant, replies: &mut Vec<(u64, Reply)>) {
        let record = self.records.get_mut(&job.record).expect("workflow record");
        record.stage = Stage::Cleanup;
        match job.phase {
            Phase::ReserveHistory => {
                if record.cleanup_error == Some("broker_settlement_unconfirmed") {
                    record.cleanup_error = None;
                }
                record.history_state = "unavailable";
                self.advance_after_history(record, now);
            }
            Phase::ReleaseHistory => {
                record.cleanup_error = Some("history_release_unavailable");
                record.stage = Stage::Cleanup;
            }
            Phase::PreflightApplication => {
                record.request_cleanup(
                    Some("application_preflight_unavailable"),
                    now + CLEANUP_DEADLINE,
                );
            }
            Phase::SnapshotDisplay | Phase::EnumerateDisplay | Phase::ConfirmDisplay => {
                record.request_cleanup(
                    Some("display_preflight_unavailable"),
                    now + CLEANUP_DEADLINE,
                );
            }
            Phase::LaunchApplication => {
                record.application_cleanup_blocked = false;
                record.application_reconcile = Some(Phase::LookupApplication);
            }
            Phase::PrepareDisplayAdmission => {
                record.request_cleanup(
                    Some("display_admission_unavailable"),
                    now + CLEANUP_DEADLINE,
                );
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
        if matches!(
            job.phase,
            Phase::ReserveHistory
                | Phase::ReleaseHistory
                | Phase::PreflightApplication
                | Phase::SnapshotDisplay
                | Phase::EnumerateDisplay
                | Phase::ConfirmDisplay
        ) {
            self.finish_or_retain(job.record, now, replies);
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
                if record.history_release_required {
                    if let Some(error) = record.cleanup_error {
                        record.fail_cleanup(error);
                        reply_failure(record, replies);
                        return;
                    }
                    record.stage = Stage::ReleaseHistory;
                    return;
                }
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
            Self::ReserveHistory => "reserve-history",
            Self::PreflightApplication => "preflight-launch",
            Self::SnapshotDisplay => "snapshot-display",
            Self::EnumerateDisplay => "enumerate-display",
            Self::ConfirmDisplay => "confirm-display",
            Self::LaunchApplication => "launch",
            Self::LookupApplication => "find-launch",
            Self::PrepareDisplayAdmission => "prepare-display-admission",
            Self::OpenRtsp => "open-rtsp",
            Self::LookupRtsp => "find-rtsp",
            Self::MonitorApplication => "application-status",
            Self::MonitorRtsp => "rtsp-status",
            Self::StopRtsp => "stop-rtsp",
            Self::VerifyRtspStopped => "verify-rtsp-stop",
            Self::StopApplication => "stop-application",
            Self::VerifyApplicationStopped => "verify-application-stop",
            Self::ReleaseHistory => "release-history",
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
    matches!(
        record.stage,
        Stage::ReserveHistory
            | Stage::PreflightApplication
            | Stage::SnapshotDisplay
            | Stage::EnumerateDisplay
            | Stage::ConfirmDisplay
            | Stage::LaunchApplication
            | Stage::PrepareDisplayAdmission
            | Stage::OpenRtsp
    ) || (record.stage == Stage::Live && now >= record.next_monitor)
        || matches!(
            record.stage,
            Stage::Cleanup
                | Stage::VerifyRtspCleanup
                | Stage::VerifyApplicationCleanup
                | Stage::ReleaseHistory
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
        Stage::ReserveHistory => Phase::ReserveHistory,
        Stage::PreflightApplication => Phase::PreflightApplication,
        Stage::SnapshotDisplay => Phase::SnapshotDisplay,
        Stage::EnumerateDisplay => Phase::EnumerateDisplay,
        Stage::ConfirmDisplay => Phase::ConfirmDisplay,
        Stage::LaunchApplication => Phase::LaunchApplication,
        Stage::PrepareDisplayAdmission => Phase::PrepareDisplayAdmission,
        Stage::OpenRtsp => Phase::OpenRtsp,
        Stage::Live if !record.desktop && record.monitor_application_next => Phase::MonitorApplication,
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
        Stage::Cleanup
            if !record.rtsp_requested
                && !record.application_requested
                && record.uncertain.is_empty()
                && record.history_release_required =>
        {
            Phase::ReleaseHistory
        }
        Stage::VerifyRtspCleanup => Phase::VerifyRtspStopped,
        Stage::VerifyApplicationCleanup => Phase::VerifyApplicationStopped,
        Stage::ReleaseHistory => Phase::ReleaseHistory,
        _ => unreachable!("scheduled record has work"),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotConfiguration {
    displays: Vec<SnapshotDisplay>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotDisplay {
    id: String,
    width: u32,
    height: u32,
    refresh: SnapshotRefresh,
    x: i32,
    y: i32,
    rotation: Option<String>,
    hdr_enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotRefresh {
    numerator: u32,
    denominator: u32,
}

fn display_snapshot(payload: &Value) -> Result<(String, Value, Vec<String>), &'static str> {
    if payload["ok"] != true {
        return Err("display_snapshot_unavailable");
    }
    let revision = payload["topology_revision"]
        .as_str()
        .filter(|value| {
            value.len() == 32
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
        .ok_or("invalid_display_snapshot")?;
    let configuration = payload
        .get("configuration")
        .ok_or("invalid_display_snapshot")?;
    if configuration["displays"]
        .as_array()
        .is_some_and(|displays| {
            displays
                .iter()
                .any(|display| display["rotation"].is_null() && display.get("rotation").is_some())
        })
    {
        return Err("invalid_display_snapshot");
    }
    let parsed: SnapshotConfiguration =
        serde_json::from_value(configuration.clone()).map_err(|_| "invalid_display_snapshot")?;
    if parsed.displays.is_empty() || parsed.displays.len() > 16 {
        return Err("invalid_display_snapshot");
    }
    let mut ids = Vec::with_capacity(parsed.displays.len());
    for display in parsed.displays {
        if display.id.is_empty()
            || display.id.len() > 512
            || display.id.chars().any(char::is_control)
            || display.width == 0
            || display.height == 0
            || display.refresh.numerator == 0
            || display.refresh.denominator == 0
            || display.rotation.as_deref().is_some_and(|rotation| {
                !matches!(
                    rotation,
                    "identity" | "rotate90" | "rotate180" | "rotate270"
                )
            })
        {
            return Err("invalid_display_snapshot");
        }
        let _ = (display.x, display.y, display.hdr_enabled);
        if ids
            .iter()
            .any(|known: &String| known.eq_ignore_ascii_case(&display.id))
        {
            return Err("invalid_display_snapshot");
        }
        ids.push(display.id);
    }
    ids.sort_by_key(|id| id.to_ascii_lowercase());
    Ok((revision.to_owned(), configuration.clone(), ids))
}

fn build_display_request(record: &Record) -> Result<Option<Value>, &'static str> {
    let snapshot = record
        .display_policy_snapshot
        .as_ref()
        .ok_or("display_policy_unavailable")?;
    let effective = snapshot["effective_overrides"]
        .as_object()
        .ok_or("display_policy_unavailable")?;
    let option = effective
        .get("dd_configuration_option")
        .and_then(Value::as_str)
        .ok_or("display_policy_unavailable")?;
    if option == "verify_only" {
        return Ok(None);
    }
    if !matches!(
        option,
        "ensure_active" | "ensure_primary" | "ensure_only_display"
    ) {
        return Err("display_mutation_unavailable");
    }
    let selected = record
        .verified_display_id
        .as_deref()
        .ok_or("display_selection_unavailable")?;
    let revision = record
        .display_topology_revision
        .as_deref()
        .ok_or("display_snapshot_unavailable")?;
    let mut configuration = record
        .display_snapshot_configuration
        .clone()
        .ok_or("display_snapshot_unavailable")?;
    let displays = configuration["displays"]
        .as_array_mut()
        .ok_or("display_snapshot_unavailable")?;
    let client_mode_override = record.peer.client_display_mode.is_some();
    let remapping = effective
        .get("dd_mode_remapping")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let has_remapping_rules = remapping.as_object().is_some_and(|rules| {
        rules.values().any(|entries| {
            entries
                .as_array()
                .is_some_and(|entries| !entries.is_empty())
        })
    });
    let resolution_option = effective
        .get("dd_resolution_option")
        .and_then(Value::as_str);
    let refresh_rate_option = effective
        .get("dd_refresh_rate_option")
        .and_then(Value::as_str);
    if effective.get("dd_hdr_option").and_then(Value::as_str) == Some("auto")
        || matches!(
            effective
                .get("dd_hdr_request_override")
                .and_then(Value::as_str),
            Some("auto" | "force_on")
        )
    {
        return Err("display_hdr_fact_unavailable");
    }
    // The current authenticated launch request carries no optimize-game-settings
    // choice. Only policies that cannot observe that choice are projected.
    if !client_mode_override
        && (has_remapping_rules
            || matches!(resolution_option, Some("auto" | "manual"))
            || effective.contains_key("dd_manual_resolution"))
    {
        return Err("display_optimize_fact_unavailable");
    }
    if !client_mode_override
        && (refresh_rate_option == Some("manual")
            || effective.contains_key("dd_manual_refresh_rate"))
    {
        return Err("display_mutation_unavailable");
    }
    if record.display_match_inactive {
        if option != "ensure_active"
            || displays.len() >= 16
            || !client_mode_override
                && matches!(refresh_rate_option, Some("auto" | "prefer_highest"))
            || !(320..=16_384).contains(&record.physical_display_mode.width)
            || !(200..=16_384).contains(&record.physical_display_mode.height)
            || displays.iter().any(|display| {
                display["id"]
                    .as_str()
                    .is_some_and(|id| id.eq_ignore_ascii_case(selected))
            })
        {
            return Err("display_mutation_unavailable");
        }
        let right = displays
            .iter()
            .try_fold(0_i64, |right, display| {
                let x = display["x"].as_i64()?;
                let width = i64::try_from(display["width"].as_u64()?).ok()?;
                Some(right.max(x.checked_add(width)?))
            })
            .ok_or("display_snapshot_unavailable")?;
        let x = i32::try_from(right).map_err(|_| "display_mutation_unavailable")?;
        if x > 65_536 {
            return Err("display_mutation_unavailable");
        }
        displays.push(json!({
            "id":selected,
            "width":record.physical_display_mode.width,
            "height":record.physical_display_mode.height,
            "refresh":{
                "numerator":record.physical_display_mode.refresh_millihz,
                "denominator":1000,
            },
            "x":x,
            "y":0,
            "rotation":"identity",
            "hdr_enabled":null,
        }));
    }
    let selected_display = displays
        .iter_mut()
        .find(|display| display["id"].as_str() == Some(selected))
        .ok_or("display_selection_unavailable")?;
    selected_display["width"] = json!(record.physical_display_mode.width);
    selected_display["height"] = json!(record.physical_display_mode.height);
    selected_display["refresh"] = json!({
        "numerator":record.physical_display_mode.refresh_millihz,
        "denominator":1000,
    });
    let mut policy = json!({
        "display_id":selected,
        "configuration_option":option,
        "remapping":remapping,
        "context":{
            "resolution_is_automatic":resolution_option == Some("auto"),
            "refresh_rate_is_automatic":matches!(refresh_rate_option, Some("auto" | "prefer_highest")),
            "client_display_mode_override":client_mode_override,
            "optimize_game_settings":false,
        },
    });
    for (source, target) in [
        ("dd_resolution_option", "resolution_option"),
        ("dd_manual_resolution", "manual_resolution"),
        ("dd_refresh_rate_option", "refresh_rate_option"),
        ("dd_manual_refresh_rate", "manual_refresh_rate"),
        ("dd_hdr_option", "hdr_option"),
    ] {
        if let Some(value) = effective.get(source) {
            policy[target] = value.clone();
        }
    }
    Ok(Some(json!({
        "configuration":configuration,
        "expected_topology_revision":revision,
        "mode_policy":policy,
    })))
}

fn request_sha256(request: &Value) -> Result<String, &'static str> {
    let bytes = serde_json::to_vec(request).map_err(|_| "display_request_unavailable")?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn display_admission_response(record: &Record, payload: &Value) -> Result<String, &'static str> {
    let request = record
        .display_request
        .as_ref()
        .ok_or("display_admission_unavailable")?;
    let hash = request_sha256(request)?;
    let token = payload["display_admission_token"]
        .as_str()
        .filter(|token| {
            token.len() == 64
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or("display_admission_unavailable")?;
    if payload["ok"] != true
        || payload.get("display_policy_snapshot") != record.display_policy_snapshot.as_ref()
        || payload["application_operation_id"] != record.application_operation
        || payload["rtsp_operation_id"] != record.rtsp_operation
        || payload["request_sha256"] != hash
    {
        return Err("display_admission_unavailable");
    }
    Ok(token.to_owned())
}

fn capture_source(record: &Record) -> Result<&str, &'static str> {
    let output = record.capture_output.as_str();
    let source = match record.capture_provider {
        super::CaptureProvider::Wgc => output,
        super::CaptureProvider::Dxgi => {
            let (luid, source) = output.split_once('/').ok_or("invalid_capture_output")?;
            let Some((high, low)) = luid.split_once(':') else {
                return Err("invalid_capture_output");
            };
            if high.len() != 8
                || low.len() != 8
                || !high
                    .bytes()
                    .chain(low.bytes())
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                return Err("invalid_capture_output");
            }
            source
        }
    };
    if source.is_empty() || source.len() > 128 || source.contains('/') {
        return Err("invalid_capture_output");
    }
    Ok(source)
}

fn display_enumeration(
    record: &mut Record,
    payload: &Value,
) -> Result<Option<usize>, &'static str> {
    if payload["ok"] != true {
        return Err("display_enumeration_unavailable");
    }
    let source = capture_source(record)?.to_owned();
    let entries = payload["entries"]
        .as_array()
        .filter(|entries| entries.len() <= 32)
        .ok_or("invalid_display_enumeration")?;
    let mut match_id = record.display_match_id.clone();
    let mut match_inactive = record.display_match_inactive;
    for entry in entries {
        let id = entry["id"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control))
            .ok_or("invalid_display_enumeration")?;
        if record
            .display_enumerated_ids
            .iter()
            .any(|known| known.eq_ignore_ascii_case(id))
        {
            return Err("ambiguous_display_identity");
        }
        record.display_enumerated_ids.push(id.to_owned());
        let entry_source = entry["source"]
            .as_str()
            .ok_or("invalid_display_enumeration")?;
        let active = entry["active"]
            .as_bool()
            .ok_or("invalid_display_enumeration")?;
        if entry_source.eq_ignore_ascii_case(&source) {
            let snapshot_id = record
                .display_snapshot_ids
                .iter()
                .find(|known| known.eq_ignore_ascii_case(id));
            if match_id.is_some() {
                return Err("ambiguous_capture_output");
            }
            if active {
                match_id = Some(snapshot_id.ok_or("capture_output_not_in_snapshot")?.clone());
            } else {
                let option = record
                    .display_policy_snapshot
                    .as_ref()
                    .and_then(|snapshot| {
                        snapshot["effective_overrides"]["dd_configuration_option"].as_str()
                    });
                if option != Some("ensure_active") || snapshot_id.is_some() {
                    return Err("inactive_capture_output");
                }
                match_id = Some(id.to_owned());
                match_inactive = true;
            }
        }
    }
    record.display_match_id = match_id;
    record.display_match_inactive = match_inactive;
    match payload.get("next_offset") {
        None | Some(Value::Null) if record.display_match_id.is_some() => Ok(None),
        None | Some(Value::Null) => Err("capture_output_not_found"),
        Some(value) => {
            let next = value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or("invalid_display_enumeration")?;
            if entries.is_empty()
                || next != record.display_enumeration_offset + entries.len()
                || next > 4096
            {
                return Err("invalid_display_enumeration");
            }
            Ok(Some(next))
        }
    }
}

fn fail_record(record: &mut Record, code: &'static str, now: Instant) {
    record.request_cleanup(Some(code), now + CLEANUP_DEADLINE);
}

fn complete_launch(record: &mut Record, now: Instant, replies: &mut Vec<(u64, Reply)>) {
    record.state = State::AwaitingDelivery;
    record.stage = Stage::AwaitingDelivery;
    if let Some(connection) = record.launch_connection {
        replies.push((connection, success_reply(record)));
    } else {
        record.request_cleanup(Some("launch_response_unavailable"), now + CLEANUP_DEADLINE);
    }
}

fn project_application_policy(
    record: &mut Record,
    application_overrides: &ApplicationConfigOverrides,
    profile: &mut super::StreamProfile,
) -> Result<(), &'static str> {
    record.scoped_ds4_back_as_touchpad_click = application_overrides.ds4_back_as_touchpad_click;
    let application_fec_percentage = application_overrides.fec_percentage;
    record.application_fec_percentage = application_fec_percentage;
    record.effective_fec_percentage = record
        .peer
        .client_fec_percentage
        .or(application_fec_percentage)
        .unwrap_or(record.global_fec_percentage);
    record.effective_max_bitrate_kbps = resolve_max_bitrate_kbps(
        record.peer.client_max_bitrate,
        application_overrides.max_bitrate,
        record.global_max_bitrate_kbps,
    );
    record.effective_minimum_fps_target = record
        .peer
        .client_minimum_fps_target
        .or(application_overrides.minimum_fps_target)
        .unwrap_or(record.global_minimum_fps_target);
    record.effective_video_max_batch_size_kb = record
        .peer
        .client_video_max_batch_size_kb
        .or(application_overrides.video_max_batch_size_kb)
        .unwrap_or(record.global_video_max_batch_size_kb);
    record.application_stream_audio = application_overrides.stream_audio;
    record.effective_stream_audio = effective_stream_audio(record, application_overrides);
    let scoped_audio_sink = record
        .peer
        .client_audio_sink
        .as_deref()
        .or(application_overrides.audio_sink.as_deref());
    // An explicit virtual selector wins over regular endpoint settings.
    // Empty means no managed selector; source host-audio fallback is
    // unavailable because Amaterasu has no host-audio toggle yet.
    record.resolved_virtual_sink = resolved_virtual_sink(record, application_overrides);
    let resolved_configured_audio_sink = if record.resolved_virtual_sink.is_some() {
        None
    } else {
        match scoped_audio_sink {
            Some("") => None,
            Some(audio_sink) => Some(audio_sink.to_owned()),
            None => record.global_configured_audio_sink.clone(),
        }
    };
    record.audio_endpoint = if record.resolved_virtual_sink.is_some() {
        String::new()
    } else {
        resolved_configured_audio_sink
            .as_deref()
            .unwrap_or(&record.bootstrap_audio_endpoint)
            .to_owned()
    };
    // A selected virtual sink wins over all regular endpoint
    // overrides. Empty scoped selectors explicitly clear inheritance.
    record.capture_sink_without_routing = capture_sink_without_routing(
        record.global_audio_sink_capture_only,
        AudioSinkCaptureOnlyOverrides {
            application: application_overrides.audio_sink_capture_only,
            client: record.peer.client_audio_sink_capture_only,
        },
        resolved_configured_audio_sink.as_deref(),
        &record.audio_endpoint,
        record.resolved_virtual_sink.as_deref(),
    );
    if record.request.audio_channels != 2
        && (record.resolved_virtual_sink.is_none() || !record.effective_stream_audio)
    {
        return Err("surround_audio_requires_stream_sink");
    }
    record.scoped_back_button_timeout = record
        .peer
        .client_back_button_timeout
        .or(application_overrides.back_button_timeout);
    record.scoped_controller = record
        .peer
        .client_controller
        .or(application_overrides.controller);
    record.scoped_gamepad = record
        .peer
        .client_gamepad
        .clone()
        .or(application_overrides.gamepad.clone());
    record.scoped_keyboard = record
        .peer
        .client_keyboard
        .or(application_overrides.keyboard);
    record.scoped_mouse = record.peer.client_mouse.or(application_overrides.mouse);
    record.scoped_native_pen_touch = record
        .peer
        .client_native_pen_touch
        .or(application_overrides.native_pen_touch);
    record.scoped_high_resolution_scrolling = record
        .peer
        .client_high_resolution_scrolling
        .or(application_overrides.high_resolution_scrolling);
    record.scoped_always_send_scancodes = record
        .peer
        .client_always_send_scancodes
        .or(application_overrides.always_send_scancodes);
    record.scoped_key_rightalt_to_key_win = record
        .peer
        .client_key_rightalt_to_key_win
        .or(application_overrides.key_rightalt_to_key_win);
    record.scoped_key_repeat_delay = record
        .peer
        .client_key_repeat_delay
        .or(application_overrides.key_repeat_delay);
    record.scoped_key_repeat_frequency = record
        .peer
        .client_key_repeat_frequency
        .or(application_overrides.key_repeat_frequency);
    profile.fec_percentage = record.effective_fec_percentage;
    if record
        .effective_max_bitrate_kbps
        .is_some_and(|bitrate| bitrate < 100)
    {
        return Err("max_bitrate_below_encoder_minimum");
    }
    profile
        .validate()
        .map_err(|_| "invalid_streaming_profile_override")
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

#[derive(Clone, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(super) struct ApplicationConfigOverrides {
    fec_percentage: Option<u8>,
    max_bitrate: Option<i32>,
    minimum_fps_target: Option<f64>,
    video_max_batch_size_kb: Option<u8>,
    dd_mode_remapping: Option<Value>,
    dd_resolution_option: Option<String>,
    dd_manual_resolution: Option<String>,
    dd_refresh_rate_option: Option<String>,
    dd_manual_refresh_rate: Option<String>,
    dd_hdr_option: Option<String>,
    dd_configuration_option: Option<String>,
    dd_hdr_request_override: Option<String>,
    stream_audio: Option<bool>,
    audio_sink: Option<String>,
    virtual_sink: Option<String>,
    audio_sink_capture_only: Option<bool>,
    back_button_timeout: Option<i32>,
    controller: Option<bool>,
    gamepad: Option<String>,
    keyboard: Option<bool>,
    mouse: Option<bool>,
    native_pen_touch: Option<bool>,
    ds4_back_as_touchpad_click: Option<bool>,
    frame_limiter_enable: Option<bool>,
    frame_limiter_fps_limit: Option<f64>,
    frame_limiter_provider: Option<String>,
    frame_limiter_disable_vsync: Option<bool>,
    frame_limiter_auto_virtual_framegen: Option<String>,
    rtss_frame_limit_type: Option<String>,
    high_resolution_scrolling: Option<bool>,
    always_send_scancodes: Option<bool>,
    key_rightalt_to_key_win: Option<bool>,
    key_repeat_delay: Option<u32>,
    key_repeat_frequency: Option<f64>,
}

fn effective_stream_audio(record: &Record, application: &ApplicationConfigOverrides) -> bool {
    record
        .peer
        .client_stream_audio
        .or(application.stream_audio)
        .unwrap_or(record.global_stream_audio)
}

fn resolved_virtual_sink(
    record: &Record,
    application: &ApplicationConfigOverrides,
) -> Option<String> {
    record
        .peer
        .client_virtual_sink
        .as_deref()
        .or(application.virtual_sink.as_deref())
        .or(record.global_configured_virtual_sink.as_deref())
        .filter(|selector| !selector.is_empty())
        .map(str::to_owned)
}

impl ApplicationConfigOverrides {
    pub(super) fn frame_limiter_settings(&self) -> FrameLimiterSettings {
        FrameLimiterSettings {
            frame_limiter_enable: self.frame_limiter_enable,
            frame_limiter_fps_limit: self.frame_limiter_fps_limit,
            frame_limiter_provider: self.frame_limiter_provider.clone(),
            frame_limiter_disable_vsync: self.frame_limiter_disable_vsync,
            frame_limiter_auto_virtual_framegen: self.frame_limiter_auto_virtual_framegen.clone(),
            rtss_frame_limit_type: self.rtss_frame_limit_type.clone(),
            ..FrameLimiterSettings::default()
        }
    }
}

pub(super) fn application_config_overrides(
    payload: &Value,
) -> Result<ApplicationConfigOverrides, &'static str> {
    let launch = payload
        .get("launch")
        .filter(|value| value.is_object())
        .ok_or("invalid_application_response")?;
    parse_application_config_overrides(launch.get("config_overrides"))
}

const APPLICATION_DISPLAY_KEYS: &[&str] = &[
    "dd_mode_remapping",
    "dd_resolution_option",
    "dd_manual_resolution",
    "dd_refresh_rate_option",
    "dd_manual_refresh_rate",
    "dd_hdr_option",
    "dd_configuration_option",
    "dd_hdr_request_override",
];

pub(super) fn application_display_overrides(config_overrides: Option<&Value>) -> Value {
    let mut display = serde_json::Map::new();
    for key in APPLICATION_DISPLAY_KEYS {
        if let Some(value) = config_overrides.and_then(|overrides| overrides.get(*key))
            && !value.is_null()
        {
            display.insert((*key).to_owned(), value.clone());
        }
    }
    Value::Object(display)
}

fn display_policy_snapshot(record: &Record) -> Value {
    let application = record
        .application_display_overrides
        .as_ref()
        .expect("display overrides captured at preflight");
    let client = &record.peer.client_display_overrides;
    let mut effective = record
        .global_display_policy
        .as_object()
        .expect("global overrides object")
        .clone();
    effective.extend(
        application
            .as_object()
            .expect("application overrides object")
            .clone(),
    );
    effective.extend(client.as_object().expect("client overrides object").clone());
    json!({
        "catalog_revision":record.application_catalog_revision,
        "global_revision":record.global_display_revision,
        "global_overrides":record.global_display_policy,
        "certificate_id":record.peer.certificate_id,
        "application_overrides":application,
        "client_overrides":client,
        "effective_overrides":effective,
    })
}

fn application_preflight_response(
    record: &Record,
    payload: &Value,
) -> Result<(String, u64, ApplicationConfigOverrides, Option<String>), &'static str> {
    if payload["ok"] != true || payload["app_id"] != app_id(record.request.app_id) {
        return Err("invalid_application_preflight_response");
    }
    let token = payload["preflight_token"]
        .as_str()
        .filter(|token| {
            !token.is_empty() && token.len() <= 256 && !token.chars().any(char::is_control)
        })
        .ok_or("invalid_application_preflight_response")?;
    let catalog_revision = payload["catalog_revision"]
        .as_u64()
        .ok_or("invalid_application_preflight_response")?;
    let overrides = parse_application_config_overrides(payload.get("config_overrides"))
        .map_err(|_| "invalid_application_preflight_response")?;
    let executable_path = payload["executable_path"].as_str().map(str::to_owned);
    if payload
        .get("executable_path")
        .is_some_and(|value| !value.is_null() && !value.is_string())
        || executable_path.as_deref().is_some_and(|path| {
            path.trim().is_empty() || path.len() > 4096 || path.chars().any(char::is_control)
        })
    {
        return Err("invalid_application_preflight_response");
    }
    Ok((
        token.to_owned(),
        catalog_revision,
        overrides,
        executable_path,
    ))
}

pub(super) fn parse_application_config_overrides(
    value: Option<&Value>,
) -> Result<ApplicationConfigOverrides, &'static str> {
    let config_overrides = match value {
        None | Some(Value::Null) => ApplicationConfigOverrides::default(),
        Some(value) => {
            serde_json::from_value(value.clone()).map_err(|_| "invalid_application_response")?
        }
    };
    if config_overrides.fec_percentage == Some(0)
        || config_overrides
            .dd_mode_remapping
            .as_ref()
            .is_some_and(|value| !value.is_object())
        || config_overrides
            .dd_resolution_option
            .as_deref()
            .is_some_and(|value| !matches!(value, "auto" | "disabled" | "manual"))
        || config_overrides
            .dd_refresh_rate_option
            .as_deref()
            .is_some_and(|value| {
                !matches!(value, "auto" | "disabled" | "manual" | "prefer_highest")
            })
        || config_overrides
            .dd_hdr_option
            .as_deref()
            .is_some_and(|value| !matches!(value, "auto" | "disabled"))
        || config_overrides
            .dd_configuration_option
            .as_deref()
            .is_some_and(|value| {
                !matches!(
                    value,
                    "disabled"
                        | "verify_only"
                        | "ensure_active"
                        | "ensure_primary"
                        | "ensure_only_display"
                )
            })
        || config_overrides
            .dd_hdr_request_override
            .as_deref()
            .is_some_and(|value| !matches!(value, "auto" | "force_on" | "force_off"))
        || config_overrides
            .minimum_fps_target
            .is_some_and(|target| !target.is_finite() || !(0.0..=1000.0).contains(&target))
        || config_overrides
            .video_max_batch_size_kb
            .is_some_and(|size| size > 64)
        || config_overrides
            .audio_sink
            .as_deref()
            .is_some_and(|value| !super::valid_audio_sink_override(value))
    {
        return Err("invalid_application_response");
    }
    if config_overrides
        .virtual_sink
        .as_deref()
        .is_some_and(|value| value.len() > 4096 || value.chars().any(char::is_control))
    {
        return Err("invalid_application_response");
    }
    if config_overrides
        .gamepad
        .as_deref()
        .is_some_and(|profile| !valid_gamepad_profile(profile))
    {
        return Err("invalid_application_response");
    }
    if config_overrides
        .key_repeat_delay
        .is_some_and(|delay| delay > i32::MAX as u32)
        || config_overrides
            .key_repeat_frequency
            .is_some_and(|frequency| !frequency.is_finite() || frequency < 0.0)
        || config_overrides
            .frame_limiter_settings()
            .validate()
            .is_err()
    {
        return Err("invalid_application_response");
    }
    Ok(config_overrides)
}

fn resolve_max_bitrate_kbps(
    client_override: Option<i32>,
    application_override: Option<i32>,
    global_override: Option<u32>,
) -> Option<u32> {
    match client_override.or(application_override) {
        Some(value) if value > 0 => Some(value as u32),
        Some(_) => None,
        None => global_override,
    }
}

fn valid_gamepad_profile(profile: &str) -> bool {
    #[cfg(windows)]
    {
        matches!(
            profile,
            "auto"
                | "x360"
                | "vhf"
                | "vhf_xbox"
                | "vhf_xbox_one"
                | "vhf_ds4"
                | "vhf_ds5"
                | "vhf_switch"
                | "ds4"
        )
    }
    #[cfg(target_os = "linux")]
    {
        matches!(profile, "auto" | "xone" | "ds4" | "ds5" | "switch")
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        profile == "auto"
    }
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
        || payload.get("frame_limiter_settings")
            != Some(
                record
                    .executable_path
                    .as_ref()
                    .and(record.frame_limiter_snapshot.as_ref())
                    .unwrap_or(&Value::Null),
            )
        || payload["frame_limiter_executable_path"].as_str() != record.executable_path.as_deref()
        || payload.get("display_policy_snapshot") != record.display_policy_snapshot.as_ref()
        || payload.get("display_request")
            != Some(record.display_request.as_ref().unwrap_or(&Value::Null))
        || (record.display_request.is_some()
            && payload["state"] == "ready"
            && payload["display_applied"] != true)
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

fn frame_limiter_launch_snapshot(record: &Record) -> Value {
    let settings = resolve_frame_limiter_settings(
        &record.global_frame_limiter_settings,
        &record.application_frame_limiter_settings,
        &record.client_frame_limiter_settings,
    );
    let requested_stream_fps_millihz = record
        .request
        .fps
        .checked_mul(1_000)
        .filter(|rate| *rate <= 1_000_000)
        .expect("admitted stream FPS must fit the frame-limiter service range");
    let explicit_rtss_override = record
        .client_frame_limiter_settings
        .rtss_frame_limit_type
        .is_some()
        || record
            .application_frame_limiter_settings
            .rtss_frame_limit_type
            .is_some();

    json!({
        "version":1,
        "settings":settings.effective(),
        "requested_stream_fps_millihz":requested_stream_fps_millihz,
        "client_display_mode_override_millihz":record.peer.client_display_mode.map(|mode| mode.refresh_millihz),
        "lossless_scaling_limit_millihz":null,
        "virtual_display_active":null,
        "physical_framegen_capture":null,
        "game_frame_generation_mode":null,
        "rtss_frame_limit_type_explicit_override":explicit_rtss_override.then_some(true)
    })
}

pub(super) fn resolve_frame_limiter_settings(
    global: &FrameLimiterSettings,
    application: &FrameLimiterSettings,
    client: &FrameLimiterSettings,
) -> FrameLimiterSettings {
    let mut settings = global.clone();
    settings.frame_limiter_enable = client
        .frame_limiter_enable
        .or(application.frame_limiter_enable)
        .or(settings.frame_limiter_enable);
    settings.frame_limiter_fps_limit = client
        .frame_limiter_fps_limit
        .or(application.frame_limiter_fps_limit)
        .or(settings.frame_limiter_fps_limit);
    settings.frame_limiter_provider = client
        .frame_limiter_provider
        .clone()
        .or(application.frame_limiter_provider.clone())
        .or(settings.frame_limiter_provider);
    settings.frame_limiter_disable_vsync = client
        .frame_limiter_disable_vsync
        .or(application.frame_limiter_disable_vsync)
        .or(settings.frame_limiter_disable_vsync);
    settings.frame_limiter_auto_virtual_framegen = client
        .frame_limiter_auto_virtual_framegen
        .clone()
        .or(application.frame_limiter_auto_virtual_framegen.clone())
        .or(settings.frame_limiter_auto_virtual_framegen);
    settings.rtss_frame_limit_type = client
        .rtss_frame_limit_type
        .clone()
        .or(application.rtss_frame_limit_type.clone())
        .or(settings.rtss_frame_limit_type);
    settings
}

fn client_frame_limiter_overrides(record: &Record) -> Value {
    let mut overrides = serde_json::to_value(&record.client_frame_limiter_settings)
        .expect("client frame-limiter overrides serialize");
    overrides
        .as_object_mut()
        .expect("client frame-limiter overrides serialize to an object")
        .retain(|_, value| !value.is_null());
    overrides
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
