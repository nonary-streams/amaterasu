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
    AcquireOutput,
    AcquireAudio,
    Activate,
    AcquireInput,
    FindInput,
    ReleaseInput,
    RenewInput,
    InputBatch,
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
        }
    }

    pub fn fail(&mut self, record: &mut Record, code: &'static str, now: u64) {
        record.error.get_or_insert(code);
        record.outcome = if code == "request_cancelled" {
            State::Cancelled
        } else {
            State::Failed
        };
        record.state = State::Stopping;
        record.plane = None;
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
            Step::AcquireOutput | Step::AcquireAudio => (
                Resource::Session.service(),
                json!({"action":"acquire","session_id":session(),"resource":record.lease(self.step == Step::AcquireAudio),"mode":"shared"}),
            ),
            Step::Activate => (
                Resource::Session.service(),
                json!({"action":"activate","session_id":session()}),
            ),
            Step::AcquireInput => (
                "amaterasu.input/1".into(),
                json!({"action":"acquire","operation_id":record.input_operation(),"ttl_ms":record.input_ttl_ms,
                    "back_button_timeout":record.start.as_ref().expect("validated start configuration").back_button_timeout}),
            ),
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
            Step::DrainVideo => (
                Resource::Video.service(),
                json!({"action":"drain","stream":record.resources[&Resource::Video]}),
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
