use super::{
    journal::Document,
    record::{Record, Resource, State},
    rpc,
};
use amaterasu_module_sdk::input::{INPUT_SERVICE, InputRequest};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    Open(Resource),
    Authenticate,
    ConfirmControlDisconnect,
    MediaReady,
    ConfigureMediaVideoFormat,
    CreateVirtualDisplay,
    StatusVirtualDisplay,
    RemoveVirtualDisplay,
    SnapshotDisplay,
    CheckDisplay,
    ApplyDisplay,
    FindDisplay,
    RestoreDisplay,
    RecoverDisplay,
    CreateOwnedVirtualDisplay,
    FindOwnedVirtualDisplay,
    RemoveOwnedVirtualDisplay,
    AcquireOutput,
    ValidateLifecycleCandidate,
    ReadFrozenProcessingSettings,
    PlanGpuProcessing,
    ObserveLifecycleCapture,
    AckLifecycleCapture,
    AckLifecycleStream,
    StartLifecycle,
    PauseLifecycle,
    RetireLifecycle,
    RecoverLifecycle,
    ObserveActiveBinding,
    ValidateActiveBinding,
    NotifyCaptureInvalidated,
    IssueLifecycleInputBinding,
    AckLifecycleInput,
    AcquireAudio,
    Activate,
    AcquireInput,
    FindInput,
    ReleaseInput,
    RenewInput,
    InputBatch,
    FeedbackPeek,
    FeedbackAck,
    PollGamepadFeedback,
    RumbleDrain,
    DrainVideo,
    CloseCapture,
    WaitDrain,
    TransportStatus,
    Find(Resource),
    Close(Resource),
    AwaitClosed(Resource),
    StopSession,
    FinishSession,
    ForceIdr,
    RecoverAudio,
    PersistJournal,
}

pub(super) struct Job {
    pub session: u64,
    pub request: Option<u64>,
    pub step: Step,
    pub pending: Option<rpc::Pending>,
    pub cleanup: bool,
    pub confirming: Option<Resource>,
    pub settling_allocation: bool,
    pub deadline: u64,
    pub tried: std::collections::BTreeSet<Resource>,
    pub input_tried: bool,
    pub virtual_display_tried: bool,
    pub virtual_display_removing: bool,
    pub display_tried: bool,
    pub journal_resume: Option<Step>,
    pub journal_document: Option<Document>,
    pub journal_payload: Option<Value>,
    pub journal_operation_id: Option<String>,
    pub rumble_tried: bool,
    pub lifecycle_pause_tried: bool,
    pub lifecycle_retire_tried: bool,
    pub lifecycle_lookup: bool,
}

impl Job {
    pub fn new(session: u64, request: Option<u64>, step: Step, deadline: u64) -> Self {
        Self {
            session,
            request,
            step,
            pending: None,
            cleanup: false,
            confirming: None,
            settling_allocation: false,
            deadline,
            tried: Default::default(),
            input_tried: false,
            virtual_display_tried: false,
            virtual_display_removing: false,
            display_tried: false,
            journal_resume: None,
            journal_document: None,
            journal_payload: None,
            journal_operation_id: None,
            rumble_tried: false,
            lifecycle_pause_tried: false,
            lifecycle_retire_tried: false,
            lifecycle_lookup: false,
        }
    }

    pub fn fail(&mut self, record: &mut Record, code: &'static str, now: u64) {
        eprintln!("amaterasu session failed step={:?} code={code}", self.step);
        record.error.get_or_insert(code);
        record.outcome = if code == "request_cancelled" {
            State::Cancelled
        } else {
            State::Failed
        };
        record.history.stopping(code);
        record.state = State::Stopping;
        super::input::queue_rumble_zeros(record, now);
        if let Some(plane) = record.plane.as_mut() {
            plane.stop();
        }
        if record.rumble_pending.is_empty()
            || record
                .start
                .as_ref()
                .is_some_and(|start| start.lifecycle_target.is_some())
        {
            record.plane = None;
        }
        record.audio_recovery_needed = false;
        if let Some(pending) = self.pending.as_mut() {
            pending.cancel();
            self.settling_allocation = true;
        }
        self.cleanup = true;
        self.lifecycle_lookup = false;
        self.deadline = now.saturating_add(15000);
    }

    pub fn next_cleanup(&mut self, record: &Record) -> Option<Step> {
        if record.control_authenticated
            && !record.control_disconnect_checked
            && matches!(
                record.error,
                Some("native_control_closed" | "native_delivery_failed_control")
            )
            && record.resources.contains_key(&Resource::Control)
        {
            return Some(Step::ConfirmControlDisconnect);
        }
        if record
            .start
            .as_ref()
            .is_some_and(|start| start.lifecycle_target.is_some())
            && !self.lifecycle_pause_tried
        {
            return Some(Step::PauseLifecycle);
        }
        if !self.rumble_tried
            && (!record.rumble_active_until.is_empty() || !record.rumble_pending.is_empty())
        {
            return Some(Step::RumbleDrain);
        }
        if !self.input_tried {
            if record.input_lookup_required {
                return Some(Step::FindInput);
            }
            if record.input_lease.is_some() {
                return Some(Step::ReleaseInput);
            }
            if record.input_attempted {
                return Some(Step::FindInput);
            }
            self.input_tried = true;
        }
        if let Some(resource) = self.confirming {
            return Some(Step::AwaitClosed(resource));
        }
        for resource in Resource::CLEANUP {
            if self.tried.contains(&resource) {
                continue;
            }
            if record.resources.contains_key(&resource) {
                return Some(Step::Close(resource));
            }
            if record.attempted.contains(&resource) {
                return Some(Step::Find(resource));
            }
            self.tried.insert(resource);
        }
        if !self.virtual_display_tried
            && record.virtual_display_attempted
            && record.config.virtual_display_intent.is_none()
        {
            return Some(if self.virtual_display_removing {
                Step::FindOwnedVirtualDisplay
            } else {
                Step::RemoveOwnedVirtualDisplay
            });
        }
        if !self.display_tried {
            if record.display_attempted && !record.display_status_verified {
                return Some(Step::FindDisplay);
            }
            if record.display_lease.is_some() {
                return Some(if record.display_state == "recovery_required" {
                    Step::RecoverDisplay
                } else {
                    Step::RestoreDisplay
                });
            }
            self.display_tried = true;
        }
        // A display Apply may have committed while its reply was lost. Never
        // release the monitor until that topology lease is verified/restored.
        if record.virtual_display_attempted
            && record.display_attempted
            && (!record.display_status_verified
                || (record.owns_display_restore && record.display_lease.is_some()))
        {
            return None;
        }
        if !self.virtual_display_tried {
            if record.virtual_display_attempted && !record.virtual_display_status_verified {
                return Some(Step::StatusVirtualDisplay);
            }
            if record.virtual_display_reference_owned {
                return Some(Step::RemoveVirtualDisplay);
            }
            if record.virtual_display_attempted {
                return Some(Step::StatusVirtualDisplay);
            }
            self.virtual_display_tried = true;
        }
        if record
            .start
            .as_ref()
            .is_some_and(|start| start.lifecycle_target.is_some())
            && record.lifecycle_fenced
            && !self.lifecycle_retire_tried
        {
            if record
                .attempted
                .iter()
                .any(|resource| *resource != Resource::Session)
            {
                return None;
            }
            if record.lifecycle_retirement_receipt.is_none() {
                return Some(Step::Find(Resource::Video));
            }
            return Some(Step::RetireLifecycle);
        }
        if record.resources.contains_key(&Resource::Session) {
            if record
                .attempted
                .iter()
                .any(|resource| *resource != Resource::Session)
            {
                return None;
            }
            return Some(Step::StopSession);
        }
        if record.attempted.contains(&Resource::Session) {
            return Some(Step::Find(Resource::Session));
        }
        if record.restart_lifecycle_target.is_some() && !self.lifecycle_retire_tried {
            if record.cleanup_error.is_some() || !record.attempted.is_empty() {
                return None;
            }
            if record.lifecycle_retirement_receipt.is_none() {
                return Some(Step::Find(Resource::Video));
            }
            return Some(Step::RecoverLifecycle);
        }
        None
    }

    pub fn command(&self, record: &Record) -> (String, Value) {
        let session = || record.resources[&Resource::Session];
        match self.step {
            Step::RecoverLifecycle => {
                let operation_id = record
                    .restart_lifecycle_operation
                    .as_ref()
                    .expect("restart operation identity");
                let payload = if self.lifecycle_lookup {
                    json!({"operation":"lookup","operation_id":operation_id})
                } else {
                    json!({"operation":"retire_stream","operation_id":operation_id,
                    "restart_recovery":true,"target":record.restart_lifecycle_target,
                    "retirement_receipt":record.lifecycle_retirement_receipt})
                };
                ("amaterasu.session-lifecycle/1".into(), payload)
            }
            Step::ReadFrozenProcessingSettings => (
                "amaterasu.session-lifecycle/1".into(),
                json!({"operation":"read_frozen_provider_settings",
                    "target":record.start.as_ref().expect("start").lifecycle_target,
                    "provider":record.processing_plan_provider.as_ref().expect("provider").provider}),
            ),
            Step::PlanGpuProcessing => {
                let plan = record.processing_plan_provider.as_ref().expect("provider");
                let (revision, settings) =
                    record.processing_frozen.as_ref().expect("frozen settings");
                (
                    plan.control_service.clone(),
                    json!({"target":record.start.as_ref().expect("start").lifecycle_target,
                    "revision":revision, "settings":settings}),
                )
            }
            Step::ObserveLifecycleCapture | Step::ObserveActiveBinding => (
                record.service(Resource::Video),
                json!({"action":"observe_capture", "stream":record.resources[&Resource::Video]}),
            ),
            Step::ValidateLifecycleCandidate
            | Step::AckLifecycleCapture
            | Step::AckLifecycleStream
            | Step::StartLifecycle
            | Step::PauseLifecycle
            | Step::RetireLifecycle
            | Step::ValidateActiveBinding
            | Step::NotifyCaptureInvalidated
            | Step::IssueLifecycleInputBinding
            | Step::AckLifecycleInput => {
                let start = record.start.as_ref().expect("lifecycle requires start");
                let target = start.lifecycle_target.as_ref().expect("lifecycle target");
                let candidate = start.lifecycle_candidate.as_ref();
                let operation = match self.step {
                    Step::ValidateLifecycleCandidate => "validate_candidate",
                    Step::AckLifecycleCapture => "ack_binding",
                    Step::AckLifecycleStream => "ack_stream",
                    Step::StartLifecycle => "start_stream",
                    Step::PauseLifecycle => "pause_stream",
                    Step::RetireLifecycle => "retire_stream",
                    Step::ValidateActiveBinding => "binding_validate",
                    Step::NotifyCaptureInvalidated => "capture_invalidated",
                    Step::IssueLifecycleInputBinding => "issue_input_binding",
                    Step::AckLifecycleInput => "ack_binding",
                    _ => unreachable!(),
                };
                let operation_id = if matches!(
                    self.step,
                    Step::ValidateActiveBinding | Step::NotifyCaptureInvalidated
                ) {
                    format!(
                        "ss-{:016x}-{operation}-{}",
                        record.id, record.lifecycle_validation_sequence
                    )
                } else if self.step == Step::AckLifecycleInput {
                    format!("ss-{:016x}-{operation}-input", record.id)
                } else {
                    format!("ss-{:016x}-{operation}", record.id)
                };
                let payload = if self.lifecycle_lookup {
                    json!({"operation":"lookup","operation_id":operation_id})
                } else {
                    let mut payload = json!({"operation":operation,"operation_id":operation_id,"target":target,"plan_revision":candidate.map(|candidate| candidate.plan_revision)});
                    match self.step {
                        Step::ValidateLifecycleCandidate => payload["candidate"] = json!(candidate),
                        Step::AckLifecycleCapture => {
                            payload["candidate"] = json!(candidate);
                            payload["consumer"] = json!("capture");
                            payload["observed_capture"] = json!(record.lifecycle_observation);
                        }
                        Step::AckLifecycleStream => payload["consumer"] = json!("capture"),
                        Step::RetireLifecycle => {
                            payload["retirement_receipt"] =
                                json!(record.lifecycle_retirement_receipt)
                        }
                        Step::ValidateActiveBinding => {
                            payload["binding"] = json!(candidate);
                            payload["observation"] = json!(record.lifecycle_observation);
                        }
                        Step::NotifyCaptureInvalidated => {
                            payload["reason"] = json!(record.lifecycle_invalid_reason);
                        }
                        Step::IssueLifecycleInputBinding => {
                            payload["consumer"] = json!("amaterasu.input/1");
                        }
                        Step::AckLifecycleInput => {
                            payload["candidate"] = json!(candidate);
                            payload["consumer"] = json!("input");
                            payload["token"] = json!(record.lifecycle_input_token);
                            payload["geometry_revision"] =
                                json!(record.lifecycle_input_geometry_revision);
                        }
                        _ => {}
                    }
                    payload
                };
                ("amaterasu.session-lifecycle/1".into(), payload)
            }
            Step::PersistJournal => (
                "amaterasu.state/1".into(),
                self.journal_payload
                    .clone()
                    .expect("journal write step owns a payload"),
            ),
            Step::Open(resource) => (record.service(resource), record.open(resource)),
            Step::Authenticate => (
                record.service(Resource::Control),
                json!({"action":"status","stream":record.resources[&Resource::Control]}),
            ),
            Step::ConfirmControlDisconnect => (
                record.service(Resource::Control),
                json!({"action":"status","stream":record.resources[&Resource::Control]}),
            ),
            Step::MediaReady => (
                record.service(Resource::Media),
                json!({"action":"status","stream":record.resources[&Resource::Media]}),
            ),
            Step::ConfigureMediaVideoFormat => {
                let start = record
                    .start
                    .as_ref()
                    .expect("validated start configuration");
                (
                    record.service(Resource::Media),
                    json!({"action":"configure_video_format",
                        "stream":record.resources[&Resource::Media],
                        "video_stream_format":start.configuration.video_stream_format}),
                )
            }
            Step::CreateOwnedVirtualDisplay => (
                "amaterasu.virtual-display/1".into(),
                super::virtual_display::owned_create(record),
            ),
            Step::FindOwnedVirtualDisplay => (
                "amaterasu.virtual-display/1".into(),
                super::virtual_display::owned_status(record),
            ),
            Step::RemoveOwnedVirtualDisplay => (
                "amaterasu.virtual-display/1".into(),
                super::virtual_display::owned_remove(record),
            ),

            Step::CreateVirtualDisplay => (
                "amaterasu.virtual-display/1".into(),
                super::virtual_display::create(record),
            ),
            Step::StatusVirtualDisplay => (
                "amaterasu.virtual-display/1".into(),
                super::virtual_display::status(record),
            ),
            Step::RemoveVirtualDisplay => (
                "amaterasu.virtual-display/1".into(),
                super::virtual_display::remove(record),
            ),
            Step::SnapshotDisplay => ("amaterasu.display/1".into(), json!({"action":"snapshot"})),
            Step::CheckDisplay | Step::FindDisplay => {
                ("amaterasu.display/1".into(), json!({"action":"status"}))
            }
            Step::ApplyDisplay => ("amaterasu.display/1".into(), super::display::apply(record)),
            Step::RestoreDisplay | Step::RecoverDisplay => (
                "amaterasu.display/1".into(),
                json!({"action":if self.step == Step::RecoverDisplay {"recover"} else {"restore"},"lease":record.display_lease}),
            ),
            Step::AcquireOutput | Step::AcquireAudio => (
                record.service(Resource::Session),
                json!({"action":"acquire","session_id":session(),"resource":record.lease(self.step == Step::AcquireAudio),"mode":"shared"}),
            ),
            Step::Activate => (
                record.service(Resource::Session),
                json!({"action":"activate","session_id":session()}),
            ),
            Step::AcquireInput => {
                let start = record
                    .start
                    .as_ref()
                    .expect("validated start configuration");
                let mut payload = json!({"action":"acquire","operation_id":record.input_operation(),"ttl_ms":record.input_ttl_ms,
                    "back_button_timeout":start.back_button_timeout,
                    "controller":start.controller,
                    "gamepad":start.gamepad,
                    "keyboard":start.keyboard,
                    "mouse":start.mouse,
                    "native_pen_touch":start.native_pen_touch,
                    "high_resolution_scrolling":start.high_resolution_scrolling,
                    "always_send_scancodes":start.always_send_scancodes,
                    "key_rightalt_to_key_win":start.key_rightalt_to_key_win,
                    "keybindings":start.keybindings,
                    "key_repeat_delay":start.key_repeat_delay,
                    "key_repeat_frequency":start.key_repeat_frequency});
                if let Some(token) = &record.lifecycle_input_token {
                    payload["lifecycle_binding_token"] = json!(token);
                }
                if let Some(back_as_touchpad_click) = start.ds4_back_as_touchpad_click {
                    payload["ds4_back_as_touchpad_click"] = json!(back_as_touchpad_click);
                }
                if let Some(motion_as_ds4) = start.motion_as_ds4 {
                    payload["motion_as_ds4"] = json!(motion_as_ds4);
                }
                if let Some(touchpad_as_ds4) = start.touchpad_as_ds4 {
                    payload["touchpad_as_ds4"] = json!(touchpad_as_ds4);
                }
                if let Some(ds5_inputtino_randomize_mac) = start.ds5_inputtino_randomize_mac {
                    payload["ds5_inputtino_randomize_mac"] = json!(ds5_inputtino_randomize_mac);
                }
                (INPUT_SERVICE.into(), payload)
            }
            Step::FindInput => (
                INPUT_SERVICE.into(),
                InputRequest::Lookup {
                    operation_id: &record.input_operation(),
                }
                .payload(),
            ),
            Step::ReleaseInput => (
                INPUT_SERVICE.into(),
                InputRequest::Release {
                    lease: record.input_lease.as_deref(),
                }
                .payload(),
            ),
            Step::RenewInput => (
                INPUT_SERVICE.into(),
                InputRequest::Renew {
                    lease: record.input_lease.as_deref(),
                    ttl_ms: record.input_ttl_ms,
                }
                .payload(),
            ),
            Step::InputBatch => {
                let binding = record
                    .lifecycle_input_token
                    .as_deref()
                    .zip(record.lifecycle_input_epoch.as_deref());
                (
                    INPUT_SERVICE.into(),
                    InputRequest::Batch {
                        lease: record.input_lease.as_deref(),
                        events: record
                            .input_events
                            .iter()
                            .take(record.input_inflight)
                            .collect(),
                        input_binding: binding.map(|(token, _)| token),
                        operation_id: binding.map(|(_, epoch)| {
                            format!("{epoch}:{}", record.lifecycle_input_sequence)
                        }),
                    }
                    .payload(),
                )
            }
            Step::FeedbackPeek => (
                INPUT_SERVICE.into(),
                InputRequest::FeedbackPeek {
                    lease: record.input_lease.as_deref(),
                    limit: 16,
                }
                .payload(),
            ),
            Step::FeedbackAck => (
                INPUT_SERVICE.into(),
                InputRequest::FeedbackAck {
                    lease: record.input_lease.as_deref(),
                    through_sequence: record.feedback_ack_through,
                }
                .payload(),
            ),
            Step::PollGamepadFeedback => (
                INPUT_SERVICE.into(),
                InputRequest::PollGamepadFeedback {
                    lease: record.input_lease.as_deref(),
                }
                .payload(),
            ),
            Step::RumbleDrain => (String::new(), Value::Null),
            Step::DrainVideo => (
                record.service(Resource::Video),
                json!({"action":"drain","stream":record.resources[&Resource::Video]}),
            ),
            Step::RecoverAudio => (
                record.service(Resource::AudioEncoder),
                json!({"action":"recover","stream":record.resources[&Resource::AudioEncoder]}),
            ),
            Step::CloseCapture => (
                record.service(Resource::AudioCapture),
                json!({"action":"close","stream":record.resources[&Resource::AudioCapture]}),
            ),
            Step::TransportStatus => (
                record.service(Resource::Media),
                json!({"action":"status","stream":record.resources[&Resource::Media]}),
            ),
            Step::Find(Resource::Session) => (
                record.service(Resource::Session),
                json!({"action":"lookup","key":record.operation(Resource::Session)}),
            ),
            Step::Find(resource) | Step::AwaitClosed(resource) => (
                record.service(resource),
                json!({"action":"lookup","operation_id":record.operation(resource)}),
            ),
            Step::Close(resource) => (
                record.service(resource),
                json!({"action":"close","stream":record.resources[&resource]}),
            ),
            Step::StopSession => (
                record.service(Resource::Session),
                json!({"action":"stop","session_id":session()}),
            ),
            Step::FinishSession => (
                record.service(Resource::Session),
                json!({"action":"finish_stop","session_id":session()}),
            ),
            Step::ForceIdr => (
                record.service(Resource::Video),
                json!({"action":"force_idr","stream":record.resources[&Resource::Video]}),
            ),
            Step::WaitDrain => unreachable!("native drain has no JSON command"),
        }
    }
}
