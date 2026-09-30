use super::{
    record::{Record, Resource, State},
    rpc,
};
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    Open(Resource),
    Authenticate,
    MediaReady,
    ConfigureMediaVideoFormat,
    ApplyLimiter,
    CreateVirtualDisplay,
    StatusVirtualDisplay,
    RemoveVirtualDisplay,
    SnapshotDisplay,
    CheckDisplay,
    ApplyDisplay,
    FindDisplay,
    RestoreDisplay,
    RecoverDisplay,
    FindLimiter,
    RestoreLimiter,
    AcquireOutput,
    AcquireAudio,
    Activate,
    AcquireInput,
    FindInput,
    ReleaseInput,
    RenewInput,
    InputBatch,
    FeedbackPeek,
    FeedbackAck,
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
    pub limiter_tried: bool,
    pub display_tried: bool,
    pub virtual_display_tried: bool,
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
            limiter_tried: false,
            display_tried: false,
            virtual_display_tried: false,
        }
    }

    pub fn fail(&mut self, record: &mut Record, code: &'static str, now: u64) {
        record.error.get_or_insert(code);
        record.outcome = if code == "request_cancelled" {
            State::Cancelled
        } else {
            State::Failed
        };
        record.history.stopping(code);
        record.state = State::Stopping;
        record.plane = None;
        record.audio_recovery_needed = false;
        if let Some(pending) = self.pending.as_mut() {
            pending.cancel();
            self.settling_allocation = true;
        }
        self.cleanup = true;
        self.deadline = now.saturating_add(15000);
    }

    pub fn next_cleanup(&mut self, record: &Record) -> Option<Step> {
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
        if !self.limiter_tried {
            if record.limiter_attempted && record.limiter_lease.is_none() {
                return Some(Step::FindLimiter);
            }
            if record.limiter_lease.is_some() {
                return Some(Step::RestoreLimiter);
            }
            self.limiter_tried = true;
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
        None
    }

    pub fn command(&self, record: &Record) -> (String, Value) {
        let session = || record.resources[&Resource::Session];
        match self.step {
            Step::Open(resource) => (resource.service(), record.open(resource)),
            Step::Authenticate => (
                Resource::Control.service(),
                json!({"action":"status","stream":record.resources[&Resource::Control]}),
            ),
            Step::MediaReady => (
                Resource::Media.service(),
                json!({"action":"status","stream":record.resources[&Resource::Media]}),
            ),
            Step::ConfigureMediaVideoFormat => {
                let start = record
                    .start
                    .as_ref()
                    .expect("validated start configuration");
                (
                    Resource::Media.service(),
                    json!({"action":"configure_video_format",
                        "stream":record.resources[&Resource::Media],
                        "video_stream_format":start.configuration.video_stream_format}),
                )
            }
            Step::ApplyLimiter => (
                "amaterasu.frame-limiter/1".into(),
                super::limiter::apply(record),
            ),
            Step::FindLimiter => (
                "amaterasu.frame-limiter/1".into(),
                json!({"action":"status","version":1,"session_id":record.limiter_session()}),
            ),
            Step::RestoreLimiter => (
                "amaterasu.frame-limiter/1".into(),
                json!({"action":"restore","version":1,"lease":record.limiter_lease}),
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
                Resource::Session.service(),
                json!({"action":"acquire","session_id":session(),"resource":record.lease(self.step == Step::AcquireAudio),"mode":"shared"}),
            ),
            Step::Activate => (
                Resource::Session.service(),
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
                ("amaterasu.input/1".into(), payload)
            }
            Step::FindInput => (
                "amaterasu.input/1".into(),
                json!({"action":"lookup","operation_id":record.input_operation()}),
            ),
            Step::ReleaseInput => (
                "amaterasu.input/1".into(),
                json!({"action":"release","lease":record.input_lease}),
            ),
            Step::RenewInput => (
                "amaterasu.input/1".into(),
                json!({"action":"renew","lease":record.input_lease,"ttl_ms":record.input_ttl_ms}),
            ),
            Step::InputBatch => (
                "amaterasu.input/1".into(),
                json!({"action":"batch","lease":record.input_lease,"events":record.input_events.iter().take(record.input_inflight).collect::<Vec<_>>() }),
            ),
            Step::FeedbackPeek => (
                "amaterasu.input/1".into(),
                json!({"action":"feedback_peek","lease":record.input_lease,"limit":16}),
            ),
            Step::FeedbackAck => (
                "amaterasu.input/1".into(),
                json!({"action":"feedback_ack","lease":record.input_lease,
                    "through_sequence":record.feedback_ack_through}),
            ),
            Step::DrainVideo => (
                Resource::Video.service(),
                json!({"action":"drain","stream":record.resources[&Resource::Video]}),
            ),
            Step::RecoverAudio => (
                Resource::AudioEncoder.service(),
                json!({"action":"recover","stream":record.resources[&Resource::AudioEncoder]}),
            ),
            Step::CloseCapture => (
                Resource::AudioCapture.service(),
                json!({"action":"close","stream":record.resources[&Resource::AudioCapture]}),
            ),
            Step::TransportStatus => (
                Resource::Media.service(),
                json!({"action":"status","stream":record.resources[&Resource::Media]}),
            ),
            Step::Find(Resource::Session) => (
                Resource::Session.service(),
                json!({"action":"lookup","key":record.operation(Resource::Session)}),
            ),
            Step::Find(resource) | Step::AwaitClosed(resource) => (
                resource.service(),
                json!({"action":"lookup","operation_id":record.operation(resource)}),
            ),
            Step::Close(resource) => (
                resource.service(),
                json!({"action":"close","stream":record.resources[&resource]}),
            ),
            Step::StopSession => (
                Resource::Session.service(),
                json!({"action":"stop","session_id":session()}),
            ),
            Step::FinishSession => (
                Resource::Session.service(),
                json!({"action":"finish_stop","session_id":session()}),
            ),
            Step::ForceIdr => (
                Resource::Video.service(),
                json!({"action":"force_idr","stream":record.resources[&Resource::Video]}),
            ),
            Step::WaitDrain => unreachable!("native drain has no JSON command"),
        }
    }
}
