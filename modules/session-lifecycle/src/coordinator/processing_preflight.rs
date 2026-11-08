//! Bounded read-only processing-policy preflight for a frozen lifecycle scope.
use amaterasu_extension_api::{LifecycleTarget, VideoMode};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, Instant};

const MAX_RESPONSE_BYTES: usize = 16 * 1024;
const MAX_SETTINGS_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProcessingDecision {
    pub target: LifecycleTarget,
    pub revision: String,
    pub enabled: bool,
    pub source_video_mode: Option<VideoMode>,
    pub processor_service: String,
}

/// A joined SDR consumer can bypass a transform when the already prepared
/// source is SDR. Other source changes still require a new session.
pub(super) fn compatible_with_session(
    session: &ProcessingDecision,
    stream: &ProcessingDecision,
    join: Option<&amaterasu_extension_api::StreamJoinContext>,
    requested_video: Option<&VideoMode>,
) -> bool {
    if session.processor_service != stream.processor_service {
        return false;
    }
    if session.enabled == stream.enabled
        && session.source_video_mode.as_ref().map(|mode| mode.hdr)
            == stream.source_video_mode.as_ref().map(|mode| mode.hdr)
    {
        return true;
    }
    let Some(join) = join else {
        return false;
    };
    session.enabled
        && session
            .source_video_mode
            .as_ref()
            .is_some_and(|mode| !mode.hdr)
        && !stream.enabled
        && stream.source_video_mode.is_none()
        && requested_video.is_some_and(|mode| !mode.hdr)
        && join
            .binding
            .as_ref()
            .is_none_or(|binding| !binding.mode.hdr)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentResponse {
    status: String,
    target: LifecycleTarget,
    revision: String,
    enabled: bool,
    source_hdr_required: Option<bool>,
    output_hdr_required: Option<bool>,
    processor_settings: Value,
    processor_service: String,
}

enum Stage {
    Submit,
    Poll(u64),
    Cancel(u64),
    Release(u64, Result<ProcessingDecision, String>),
    Complete,
    Failed,
}

/// The job owns one broker request, consumes and releases its result before
/// returning the decision, and never issues a state-changing provider command.
pub(super) struct ProcessingPreflightJob {
    service: String,
    interface_minor: u32,
    processor_service: String,
    target: LifecycleTarget,
    revision: String,
    settings: Value,
    requested_video: Option<VideoMode>,
    deadline: Instant,
    stage: Stage,
}

impl ProcessingPreflightJob {
    pub(super) fn new(
        service: String,
        interface_minor: u32,
        processor_service: String,
        target: LifecycleTarget,
        revision: String,
        settings: Value,
        requested_video: Option<VideoMode>,
    ) -> ModuleResult<Self> {
        if interface_minor != 1
            || service.is_empty()
            || service.len() > 128
            || processor_service.is_empty()
            || processor_service.len() > 128
            || revision.is_empty()
            || revision.len() > 128
            || revision.chars().any(char::is_control)
            || !settings.is_object()
            || !serde_json::to_vec(&settings).is_ok_and(|bytes| bytes.len() <= MAX_SETTINGS_BYTES)
        {
            return Err("invalid_processing_preflight".into());
        }
        Ok(Self {
            service,
            interface_minor,
            processor_service,
            target,
            revision,
            settings,
            requested_video,
            deadline: Instant::now() + Duration::from_secs(6),
            stage: Stage::Submit,
        })
    }

    /// Begin retiring a request after its lifecycle work stops being pending.
    pub(super) fn cancel(&mut self) -> bool {
        match self.stage {
            Stage::Submit | Stage::Complete | Stage::Failed => true,
            Stage::Poll(request) => {
                self.stage = Stage::Cancel(request);
                false
            }
            Stage::Cancel(_) | Stage::Release(_, _) => false,
        }
    }

    pub(super) fn advance(
        &mut self,
        send: &mut (impl FnMut(Command) -> ModuleResult<Response> + ?Sized),
    ) -> ModuleResult<Option<ProcessingDecision>> {
        match &self.stage {
            Stage::Submit => {
                let operation_id = amaterasu_storage::random_identifier()
                    .map_err(|_| "processing preflight entropy unavailable")?;
                let request = Command::CallApi {
                    service: self.service.clone(),
                    method: "processing_intent".into(),
                    interface_major: 1,
                    interface_minor: self.interface_minor,
                    payload: amaterasu_module_sdk::requests::processing_intent_payload(
                        &self.target,
                        &self.revision,
                        &self.settings,
                        self.requested_video.as_ref().map(|video| video.hdr),
                    )?,
                    timeout_ms: 5000,
                    operation_id,
                    parent: None,
                };
                match send(request)? {
                    Response::Accepted { id } => self.stage = Stage::Poll(id),
                    _ => return Err("processing_preflight_unavailable".into()),
                }
                Ok(None)
            }
            Stage::Poll(request) => {
                let request = *request;
                if Instant::now() >= self.deadline {
                    self.stage = Stage::Cancel(request);
                    return Ok(None);
                }
                match send(Command::TakeResult { request }) {
                    Ok(Response::Pending) => {}
                    Ok(Response::Result { payload }) => {
                        self.stage = Stage::Release(request, self.parse_response(payload));
                    }
                    Ok(Response::Error { code, .. })
                        if code == "deadline_exceeded" || code == "cancelled" =>
                    {
                        self.stage =
                            Stage::Release(request, Err("processing_preflight_unavailable".into()));
                    }
                    _ => self.stage = Stage::Cancel(request),
                }
                Ok(None)
            }
            Stage::Cancel(request) => {
                let request = *request;
                // Do not abandon an accepted broker result on an ambiguous poll.
                // The request is read-only, so cancel before releasing its record.
                match send(Command::Cancel { request }) {
                    Ok(Response::Ok) => {
                        self.stage =
                            Stage::Release(request, Err("processing_preflight_unavailable".into()))
                    }
                    Ok(Response::Error { code, .. }) if code == "not_found" => {
                        self.stage = Stage::Failed;
                        return Err("processing_preflight_unavailable".into());
                    }
                    _ => {} // Retain the job and retry cleanup on the next tick.
                }
                Ok(None)
            }
            Stage::Release(request, _) => {
                let request = *request;
                match send(Command::ReleaseResult { request }) {
                    Ok(Response::Ok) => {}
                    Ok(Response::Error { code, .. }) if code == "not_found" => {}
                    _ => return Ok(None), // Retain ownership until release is confirmed.
                }
                match std::mem::replace(&mut self.stage, Stage::Complete) {
                    Stage::Release(_, Ok(decision)) => Ok(Some(decision)),
                    Stage::Release(_, Err(error)) => {
                        self.stage = Stage::Failed;
                        Err(error)
                    }
                    _ => unreachable!(),
                }
            }
            Stage::Complete | Stage::Failed => Err("processing_preflight_finished".into()),
        }
    }

    fn parse_response(&self, payload: Value) -> Result<ProcessingDecision, String> {
        if !serde_json::to_vec(&payload).is_ok_and(|bytes| bytes.len() <= MAX_RESPONSE_BYTES) {
            return Err("invalid_processing_intent".into());
        }
        let response: IntentResponse =
            serde_json::from_value(payload).map_err(|_| "invalid_processing_intent")?;
        if response.status != "succeeded"
            || response.target != self.target
            || response.revision != self.revision
            || amaterasu_extension_api::validate_processing_configuration(
                &self.settings,
                &response.processor_settings,
            )
            .is_err()
            || response.processor_service != self.processor_service
            || (!response.enabled
                && (response.source_hdr_required.is_some()
                    || response.output_hdr_required.is_some()))
        {
            return Err("invalid_processing_intent".into());
        }
        if response.enabled
            && (response.source_hdr_required.is_none() || response.output_hdr_required.is_none())
        {
            return Err("invalid_processing_intent".into());
        }
        let source_video_mode = match (
            &self.requested_video,
            response.source_hdr_required,
            response.output_hdr_required,
        ) {
            (Some(_), None, None) => None,
            (Some(video), source, output) => {
                if output.is_some_and(|required| required != video.hdr) {
                    return Err("processing_output_range_mismatch".into());
                }
                let mut source_video = video.clone();
                if let Some(required) = source {
                    source_video.hdr = required;
                }
                (source_video != *video).then_some(source_video)
            }
            (None, None, None) => None,
            (None, _, _) => return Err("processing_video_mode_missing".into()),
        };
        Ok(ProcessingDecision {
            target: self.target.clone(),
            revision: self.revision.clone(),
            enabled: response.enabled,
            source_video_mode,
            processor_service: response.processor_service,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::VecDeque;

    fn fixture() -> (ProcessingPreflightJob, Value) {
        let target = LifecycleTarget::Session {
            host_epoch: "epoch".into(),
            session_id: "session".into(),
            generation: 1,
        };
        let settings = json!({"enabled":true});
        let response = json!({"status":"succeeded","target":target,"revision":"rev","enabled":true,
            "source_hdr_required":false,"output_hdr_required":true,"processor_settings":settings,"processor_service":"processor/1"});
        let job = ProcessingPreflightJob::new(
            "policy/1".into(),
            1,
            "processor/1".into(),
            target,
            "rev".into(),
            settings,
            Some(VideoMode {
                width: 1920,
                height: 1080,
                refresh_millihz: 60000,
                hdr: true,
            }),
        )
        .unwrap();
        (job, response)
    }

    #[test]
    fn sdr_join_can_bypass_only_a_compatible_retained_sdr_source() {
        let (job, response) = fixture();
        let session = job.parse_response(response).unwrap();
        let mut stream = session.clone();
        stream.enabled = false;
        stream.source_video_mode = None;
        let source = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"source","generation":1});
        let join: amaterasu_extension_api::StreamJoinContext = serde_json::from_value(json!({
            "source":source,"source_transition":"transition","capture_binding_mode":"external",
            "binding_owner":null,"binding":null
        }))
        .unwrap();
        let mut requested = session.source_video_mode.clone().unwrap();
        assert!(compatible_with_session(
            &session,
            &stream,
            Some(&join),
            Some(&requested)
        ));
        assert!(!compatible_with_session(
            &session,
            &stream,
            None,
            Some(&requested)
        ));
        requested.hdr = true;
        assert!(!compatible_with_session(
            &session,
            &stream,
            Some(&join),
            Some(&requested)
        ));
        requested.hdr = false;
        stream.processor_service = "another/1".into();
        assert!(!compatible_with_session(
            &session,
            &stream,
            Some(&join),
            Some(&requested)
        ));
        stream.processor_service = session.processor_service.clone();
        let mut incompatible_session = session.clone();
        incompatible_session.source_video_mode.as_mut().unwrap().hdr = true;
        assert!(!compatible_with_session(
            &incompatible_session,
            &stream,
            Some(&join),
            Some(&requested)
        ));
    }

    #[test]
    fn provider_decision_drives_source_range_and_releases_broker_result() {
        let (mut job, payload) = fixture();
        let mut replies = VecDeque::from([
            Response::Accepted { id: 42 },
            Response::Pending,
            Response::Result { payload },
            Response::Ok,
        ]);
        let mut kinds = vec![];
        let mut send = |command| {
            kinds.push(match command {
                Command::CallApi {
                    service,
                    method,
                    interface_major,
                    interface_minor,
                    payload,
                    ..
                } => {
                    assert_eq!(
                        (
                            service.as_str(),
                            method.as_str(),
                            interface_major,
                            interface_minor
                        ),
                        ("policy/1", "processing_intent", 1, 1)
                    );
                    assert_eq!(payload["revision"], "rev");
                    assert_eq!(payload["settings"], json!({"enabled":true}));
                    assert_eq!(payload["target"]["session_id"], "session");
                    "request"
                }
                Command::TakeResult { .. } => "take",
                Command::ReleaseResult { .. } => "release",
                _ => "other",
            });
            Ok(replies.pop_front().unwrap())
        };
        assert!(job.advance(&mut send).unwrap().is_none());
        assert!(job.advance(&mut send).unwrap().is_none());
        assert!(job.advance(&mut send).unwrap().is_none());
        let decision = job.advance(&mut send).unwrap().unwrap();
        assert!(!decision.source_video_mode.unwrap().hdr);
        assert_eq!(kinds, ["request", "take", "take", "release"]);
    }

    #[test]
    fn version_one_one_supplies_output_range_before_effect_enablement() {
        let (mut job, mut response) = fixture();
        job.settings = json!({"enabled":false});
        response["processor_settings"] = job.settings.clone();
        let mut send = |command| match command {
            Command::CallApi {
                interface_minor,
                payload,
                ..
            } => {
                assert_eq!(interface_minor, 1);
                assert_eq!(payload["output_hdr"], true);
                assert_eq!(payload["settings"]["enabled"], false);
                Ok(Response::Accepted { id: 4 })
            }
            Command::TakeResult { .. } => Ok(Response::Result {
                payload: response.clone(),
            }),
            Command::ReleaseResult { .. } => Ok(Response::Ok),
            _ => unreachable!(),
        };
        assert!(job.advance(&mut send).unwrap().is_none());
        assert!(job.advance(&mut send).unwrap().is_none());
        let decision = job.advance(&mut send).unwrap().unwrap();
        assert!(
            decision.enabled,
            "effect-off still selects the compatible pipeline"
        );
        assert!(!decision.source_video_mode.unwrap().hdr);
    }

    #[test]
    fn rejects_forgery_incompatible_output_and_unbounded_response() {
        let (job, valid) = fixture();
        for (field, value) in [
            ("revision", json!("stale")),
            (
                "target",
                json!({"kind":"session","host_epoch":"epoch","session_id":"other","generation":1}),
            ),
            ("processor_service", json!("other/1")),
            ("processor_settings", json!({"enabled":false})),
            ("output_hdr_required", json!(false)),
            ("source_hdr_required", Value::Null),
        ] {
            let mut payload = valid.clone();
            payload[field] = value;
            assert!(job.parse_response(payload).is_err(), "field {field}");
        }
        let mut oversized = valid;
        oversized["unused"] = json!("x".repeat(MAX_RESPONSE_BYTES));
        assert!(job.parse_response(oversized).is_err());
    }

    #[test]
    fn provider_context_enrichment_preserves_frozen_policy_and_target_checks() {
        for enabled in [false, true] {
            let (job, mut payload) = fixture();
            payload["enabled"] = json!(enabled);
            if !enabled {
                payload["source_hdr_required"] = Value::Null;
                payload["output_hdr_required"] = Value::Null;
            }
            payload["processor_settings"]["foreground_target"] = payload["target"].clone();
            assert!(job.parse_response(payload.clone()).is_ok());
            payload["processor_settings"]["enabled"] = json!(false);
            assert!(job.parse_response(payload).is_err());
        }
    }

    #[test]
    fn ambiguous_poll_cancels_and_retries_release_before_failure() {
        let (mut job, _) = fixture();
        let mut replies = VecDeque::from([
            Ok(Response::Accepted { id: 42 }),
            Err("transport_failed".into()),
            Ok(Response::Ok),
            Err("temporary_failure".into()),
            Ok(Response::Ok),
        ]);
        let mut commands = Vec::new();
        let mut send = |command| {
            commands.push(match command {
                Command::CallApi { .. } => "request",
                Command::TakeResult { .. } => "take",
                Command::Cancel { .. } => "cancel",
                Command::ReleaseResult { .. } => "release",
                _ => "other",
            });
            replies.pop_front().unwrap()
        };
        for _ in 0..4 {
            assert!(job.advance(&mut send).unwrap().is_none());
        }
        assert!(job.advance(&mut send).is_err());
        assert_eq!(
            commands,
            ["request", "take", "cancel", "release", "release"]
        );
    }

    #[test]
    fn abandoning_pending_work_cancels_and_releases_accepted_request() {
        let (mut job, _) = fixture();
        let mut replies =
            VecDeque::from([Response::Accepted { id: 42 }, Response::Ok, Response::Ok]);
        let mut commands = Vec::new();
        let mut send = |command| {
            commands.push(match command {
                Command::CallApi { .. } => "call",
                Command::Cancel { request: 42 } => "cancel",
                Command::ReleaseResult { request: 42 } => "release",
                _ => "unexpected",
            });
            Ok(replies.pop_front().unwrap())
        };
        assert!(job.advance(&mut send).unwrap().is_none());
        assert!(!job.cancel());
        assert!(job.advance(&mut send).unwrap().is_none());
        assert!(job.advance(&mut send).is_err());
        assert_eq!(commands, ["call", "cancel", "release"]);
    }

    #[test]
    fn disabled_effect_preserves_original_mode() {
        let (job, mut payload) = fixture();
        payload["enabled"] = json!(false);
        payload["source_hdr_required"] = Value::Null;
        payload["output_hdr_required"] = Value::Null;
        assert_eq!(job.parse_response(payload).unwrap().source_video_mode, None);
    }
}
