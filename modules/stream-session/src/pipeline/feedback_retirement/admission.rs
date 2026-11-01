//! Test-gated selected-owner admission. The source is reserved locally before
//! NVHTTP or pairing can commit an operation.
use super::{
    Send,
    registry::{Binding, Registry},
    worker::Call,
};
use crate::pipeline::record::Record;
use amaterasu_extension_api::LifecycleTarget;
use amaterasu_module_sdk::{
    effect_authority::{self, FeedbackCapability, Source, StreamScope},
    nvhttp_source::BindFeedbackSource,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
struct Candidate {
    session: u64,
    slot: u16,
    source_label: Value,
    observation: Value,
    binding: Binding,
    scope: StreamScope,
}
impl Candidate {
    fn find(
        records: &BTreeMap<u64, Record>,
        registry: &Registry,
        retired: &[Binding],
    ) -> Option<Self> {
        for record in records.values() {
            if !record.client_haptics_requested || !record.input_controller_supported {
                continue;
            }
            let Some(target) = record
                .start
                .as_ref()
                .and_then(|start| start.lifecycle_target.clone())
            else {
                continue;
            };
            let Some(token) = record.lifecycle_input_token.clone() else {
                continue;
            };
            let Some((stream, service, epoch)) = record.control_retirement() else {
                continue;
            };
            for (slot, observation) in &record.feedback_source_observations {
                let Some(boot) = observation["input_boot_epoch"].as_str() else {
                    continue;
                };
                let binding = Binding {
                    token: token.clone(),
                    target: target.clone(),
                    input_boot_epoch: boot.into(),
                };
                if registry.has_slot(&binding, *slot) || retired.contains(&binding) {
                    continue;
                }
                let Ok(capabilities) = serde_json::from_value::<BTreeSet<FeedbackCapability>>(
                    observation["scope_capabilities"].clone(),
                ) else {
                    continue;
                };
                let scope = StreamScope {
                    capabilities,
                    controller_slot: *slot,
                    transport_service: service.into(),
                    transport_stream: stream,
                    transport_epoch: epoch.into(),
                };
                if !scope.valid() {
                    continue;
                }
                return Some(Self {
                    session: record.id,
                    slot: *slot,
                    source_label: observation["source"].clone(),
                    observation: observation.clone(),
                    binding,
                    scope,
                });
            }
        }
        None
    }
    fn current(&self, records: &BTreeMap<u64, Record>) -> bool {
        records.get(&self.session).is_some_and(|record| {
            record.client_haptics_requested
                && record.input_controller_supported
                && record.lifecycle_input_token.as_deref() == Some(self.binding.token.as_str())
                && record
                    .start
                    .as_ref()
                    .and_then(|start| start.lifecycle_target.as_ref())
                    == Some(&self.binding.target)
                && record.feedback_source_observations.get(&self.slot) == Some(&self.observation)
                && record.control_retirement()
                    == Some((
                        self.scope.transport_stream,
                        self.scope.transport_service.as_str(),
                        self.scope.transport_epoch.as_str(),
                    ))
        })
    }
    fn source(&self, owner_epoch: String) -> Option<Source> {
        let LifecycleTarget::Stream {
            host_epoch,
            session_id,
            stream_id,
            generation,
        } = &self.binding.target
        else {
            return None;
        };
        let source = Source {
            version: u8::try_from(self.source_label["version"].as_u64()?).ok()?,
            id: self.source_label["id"].as_str()?.into(),
            generation: self.source_label["generation"].as_u64()?,
            host_epoch: host_epoch.clone(),
            session_id: session_id.clone(),
            stream_id: stream_id.clone(),
            stream_generation: *generation,
            device: format!("{}.controller.{}", self.binding.input_boot_epoch, self.slot),
            device_generation: self.observation["device"]["generation"].as_u64()?,
            effect_owner_epoch: owner_epoch,
        };
        source.valid().then_some(source)
    }
}

enum Phase {
    Status,
    Bind,
    Admit,
}
pub(super) struct Admission {
    candidate: Candidate,
    sequence: u64,
    phase: Phase,
    call: Option<Call>,
    join: Option<BindFeedbackSource>,
    admit: Option<effect_authority::Request>,
}
impl Admission {
    pub fn find(
        records: &BTreeMap<u64, Record>,
        registry: &Registry,
        sequence: u64,
        retired: &[Binding],
    ) -> Option<Self> {
        if sequence == 0 {
            return None;
        }
        Some(Self {
            candidate: Candidate::find(records, registry, retired)?,
            sequence,
            phase: Phase::Status,
            call: None,
            join: None,
            admit: None,
        })
    }
    pub fn belongs_to(&self, transport: &super::registry::Transport) -> bool {
        self.candidate.scope.transport_service == transport.service
            && self.candidate.scope.transport_stream == transport.stream
            && self.candidate.scope.transport_epoch == transport.epoch
    }
    pub fn settle(&mut self, send: &mut Send<'_>) -> bool {
        self.call.as_mut().is_none_or(|call| call.settle(send))
    }
    /// `true` means that the exact reservation is admitted. A stale or
    /// uncertain operation is retained, never replayed as a new identity.
    pub fn advance(
        &mut self,
        records: &BTreeMap<u64, Record>,
        registry: &mut Registry,
        send: &mut Send<'_>,
    ) -> bool {
        if !self.candidate.current(records) {
            return self.join.is_none() && self.settle(send);
        }
        if self.call.is_none() {
            let call = match self.phase {
                Phase::Status => Call::new(
                    effect_authority::SERVICE,
                    json!({"action":"status"}),
                    format!(
                        "feedback-status-{}-{}",
                        self.candidate.session, self.candidate.slot
                    ),
                ),
                Phase::Bind => {
                    let Some(join) = self.join.as_ref() else {
                        return false;
                    };
                    Call::new(
                        amaterasu_module_sdk::nvhttp_source::SERVICE,
                        join.payload(),
                        format!("{}-bind", join.retirement_operation_id()),
                    )
                }
                Phase::Admit => {
                    let (Some(join), Some(admit)) = (self.join.as_ref(), self.admit.as_ref())
                    else {
                        return false;
                    };
                    let Ok(payload) = serde_json::to_value(admit) else {
                        return false;
                    };
                    Call::new(
                        effect_authority::SERVICE,
                        payload,
                        format!("{}-admit", join.retirement_operation_id()),
                    )
                }
            };
            self.call = Some(call);
        }
        let Some(response) = self.call.as_mut().and_then(|call| call.poll(send)) else {
            return false;
        };
        self.call = None;
        match self.phase {
            Phase::Status => {
                if response["ok"] != true
                    || response["fenced"] != false
                    || response["drain_required"] != false
                    || response["startup_recovery_required"] != false
                {
                    return false;
                }
                let Some(epoch) = response["epoch"].as_str() else {
                    return false;
                };
                let Some(owner) = response["owner_epoch"].as_str() else {
                    return false;
                };
                let Some(source) = self.candidate.source(owner.into()) else {
                    return false;
                };
                let join = BindFeedbackSource {
                    authority_epoch: epoch.into(),
                    source: source.clone(),
                    scope: self.candidate.scope.clone(),
                    input_boot_epoch: self.candidate.binding.input_boot_epoch.clone(),
                };
                if !join.valid() {
                    return false;
                }
                let operation = format!("{}-lease", join.retirement_operation_id());
                let Ok(admit) = registry.reserve(
                    epoch.into(),
                    source,
                    self.candidate.scope.clone(),
                    self.candidate.binding.clone(),
                    operation,
                    self.sequence,
                ) else {
                    return false;
                };
                self.join = Some(join);
                self.admit = Some(admit);
                self.phase = Phase::Bind;
            }
            Phase::Bind => {
                if self
                    .join
                    .as_ref()
                    .is_some_and(|join| join.accepts_confirmation(&response))
                {
                    self.phase = Phase::Admit;
                }
            }
            Phase::Admit => {
                if self
                    .join
                    .as_ref()
                    .is_some_and(|join| registry.accept_admission(&join.source, &response).is_ok())
                {
                    return true;
                }
            }
        }
        false
    }
}
