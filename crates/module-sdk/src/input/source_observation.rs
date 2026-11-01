//! Bounded, opt-in active lifecycle observation for the selected input owner.
//! The result is a point-in-time observation, never an effect grant.
use crate::{Command, ModuleResult, Response, input::extensions::source_label};
use amaterasu_extension_api::LifecycleTarget;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const LIFECYCLE: &str = "amaterasu.session-lifecycle/1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
/// Versioned request for a lifecycle-bound device observation.
pub struct Request {
    pub action: String,
    pub version: u8,
    pub lease: String,
    pub input_binding: String,
    pub controller_slot: u16,
}
impl Request {
    pub fn authorized(&self, selected_owner: &str, caller: &str, initiator: &str) -> bool {
        caller == selected_owner && initiator == caller && self.valid()
    }
    pub fn valid(&self) -> bool {
        self.action == "observe_feedback_source"
            && self.version == 1
            && self.controller_slot < 16
            && [&self.lease, &self.input_binding]
                .iter()
                .all(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
    }
}

/// Device state the provider must revalidate while the job runs.
pub struct DeviceObservation {
    pub device: (u64, u32),
    pub offer: Vec<String>,
    pub immutable_controller_policy: bool,
}

/// Bounded lifecycle validation job; its result is an observation, not a grant.
pub struct Job {
    pub request_id: u64,
    pub caller: String,
    pub request: Request,
    pub target: LifecycleTarget,
    pub observation: DeviceObservation,
    boot_epoch: String,
    deadline: Instant,
    operation_id: String,
    pending: Option<u64>,
}

impl Job {
    pub fn new(
        request_id: u64,
        caller: String,
        request: Request,
        target: LifecycleTarget,
        observation: DeviceObservation,
        boot_epoch: String,
        deadline: Instant,
    ) -> Result<Self, &'static str> {
        if !request.valid()
            || observation.device.0 == 0
            || observation.offer.len() > 32
            || observation
                .offer
                .iter()
                .any(|schema| schema.len() > 128 || schema.is_empty())
            || boot_epoch.len() != 32
            || !boot_epoch
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid_source_observation");
        }
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|_| "randomness_unavailable")?;
        let operation_id = format!(
            "input-source-{}",
            random
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        let deadline = deadline.min(Instant::now() + Duration::from_secs(5));
        Ok(Self {
            request_id,
            caller,
            request,
            target,
            observation,
            boot_epoch,
            deadline,
            operation_id,
            pending: None,
        })
    }
    pub fn cancel(&mut self, send: &mut impl FnMut(Command) -> ModuleResult<Response>) {
        if let Some(request) = self.pending.take() {
            let _ = send(Command::Cancel { request });
            let _ = send(Command::ReleaseResult { request });
        }
    }
    /// `current` is recomputed by the owner around each async operation and must
    /// include its lease, active binding, owned device and exact incarnation.
    pub fn advance(
        &mut self,
        current: bool,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Result<Option<Value>, &'static str> {
        if !current
            || Instant::now() >= self.deadline
            || !matches!(
                send(Command::CheckRequest {
                    request: self.request_id
                }),
                Ok(Response::Ok)
            )
        {
            self.cancel(send);
            return Err("source_observation_retired");
        }
        if let Some(request) = self.pending {
            return match send(Command::TakeResult { request }) {
                Ok(Response::Pending) => Ok(None),
                Ok(Response::Result { payload }) => {
                    if !matches!(send(Command::ReleaseResult { request }), Ok(Response::Ok)) {
                        return Err("source_observation_unavailable");
                    }
                    self.pending = None;
                    let source = source_label::observe(&payload, &self.target)
                        .map_err(|_| "source_observation_denied")?
                        .ok_or("source_observation_denied")?;
                    Ok(Some(json!({"ok":true,"version":1,"target":self.target,
                        "source":{"version":source.version,"id":source.id,"generation":source.generation},
                        "input_boot_epoch":self.boot_epoch,
                        "device":{"slot":self.request.controller_slot,"generation":self.observation.device.0,"capabilities":self.observation.device.1},"provider_offer":self.observation.offer,"immutable_controller_policy":self.observation.immutable_controller_policy})))
                }
                _ => Err("source_observation_unavailable"),
            };
        }
        let remaining = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        match send(Command::Request {
            service: LIFECYCLE.into(),
            payload: json!({"operation":"validate_input_binding","operation_id":self.operation_id,"token":self.request.input_binding,"input_boot_epoch":self.boot_epoch,"source_version":1}),
            timeout_ms: remaining.max(1),
            operation_id: self.operation_id.clone(),
            parent: None,
        }) {
            Ok(Response::Accepted { id }) => {
                self.pending = Some(id);
                Ok(None)
            }
            _ => Err("source_observation_unavailable"),
        }
    }
}
