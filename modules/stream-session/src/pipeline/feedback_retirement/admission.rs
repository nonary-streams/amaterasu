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
    Recover,
    Bind,
    Admit,
}

struct Recovery {
    epoch: String,
    previous_owner: String,
    next_owner: String,
    fence_generation: u64,
    attempt: u64,
    retry_after: Option<std::time::Instant>,
}

pub(super) struct Admission {
    candidate: Candidate,
    sequence: u64,
    protocol_service: String,
    phase: Phase,
    call: Option<Call>,
    status_attempt: u64,
    recovery: Option<Recovery>,
    join: Option<BindFeedbackSource>,
    admit: Option<effect_authority::Request>,
}
impl Admission {
    pub fn find(
        records: &BTreeMap<u64, Record>,
        registry: &Registry,
        sequence: u64,
        retired: &[Binding],
        protocol_service: &str,
    ) -> Option<Self> {
        if sequence == 0 {
            return None;
        }
        Some(Self {
            candidate: Candidate::find(records, registry, retired)?,
            sequence,
            protocol_service: protocol_service.into(),
            phase: Phase::Status,
            call: None,
            status_attempt: 0,
            recovery: None,
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
        if self
            .recovery
            .as_ref()
            .and_then(|recovery| recovery.retry_after)
            .is_some_and(|deadline| std::time::Instant::now() < deadline)
        {
            return false;
        }
        if !self.candidate.current(records) {
            return self.join.is_none() && self.settle(send);
        }
        if self.call.is_none() {
            let call = match self.phase {
                Phase::Status => {
                    self.status_attempt = match self.status_attempt.checked_add(1) {
                        Some(attempt) => attempt,
                        None => return false,
                    };
                    Call::new(
                        effect_authority::SERVICE,
                        json!({"action":"status"}),
                        format!(
                            "feedback-status-{}-{}-{}",
                            self.candidate.session, self.candidate.slot, self.status_attempt
                        ),
                    )
                }
                Phase::Recover => {
                    let Some(recovery) = self.recovery.as_mut() else {
                        return false;
                    };
                    recovery.attempt = match recovery.attempt.checked_add(1) {
                        Some(attempt) => attempt,
                        None => return false,
                    };
                    let request = effect_authority::Request::Recover {
                        epoch: recovery.epoch.clone(),
                        owner_epoch: recovery.next_owner.clone(),
                    };
                    let Ok(payload) = serde_json::to_value(request) else {
                        return false;
                    };
                    Call::new(
                        effect_authority::SERVICE,
                        payload,
                        format!(
                            "feedback-recover-{}-{}-{}-{}",
                            self.candidate.session,
                            self.candidate.slot,
                            recovery.fence_generation,
                            recovery.attempt
                        ),
                    )
                }
                Phase::Bind => {
                    let Some(join) = self.join.as_ref() else {
                        return false;
                    };
                    Call::new(
                        &self.protocol_service,
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
                if response["ok"] != true || response["startup_recovery_required"] != false {
                    return false;
                }
                let Some(epoch) = response["epoch"]
                    .as_str()
                    .filter(|epoch| valid_epoch(epoch))
                else {
                    return false;
                };
                let Some(owner) = response["owner_epoch"]
                    .as_str()
                    .filter(|owner| valid_owner(owner))
                else {
                    return false;
                };
                if let Some(recovery) = &self.recovery {
                    if epoch == recovery.epoch && owner == recovery.previous_owner {
                        if !quiescent_fence(&response) {
                            return false;
                        }
                        if let Some(recovery) = &mut self.recovery {
                            recovery.retry_after = None;
                        }
                        self.phase = Phase::Recover;
                        return false;
                    }
                    if epoch != recovery.epoch
                        && owner == recovery.next_owner
                        && response["fenced"] == false
                        && response["drain_required"] == false
                        && response["in_flight"] == 0
                        && response["bound_sources"] == 0
                        && response["pending_source_retirements"] == 0
                    {
                        self.recovery = None;
                    } else {
                        return false;
                    }
                } else if response["fenced"] == true {
                    if !quiescent_fence(&response) {
                        return false;
                    }
                    let mut random = [0_u8; 16];
                    if getrandom::fill(&mut random).is_err() {
                        return false;
                    }
                    let next_owner = random.iter().map(|byte| format!("{byte:02x}")).collect();
                    let Some(fence_generation) = response["fence_generation"]
                        .as_u64()
                        .filter(|generation| *generation > 0)
                    else {
                        return false;
                    };
                    self.recovery = Some(Recovery {
                        epoch: epoch.into(),
                        previous_owner: owner.into(),
                        next_owner,
                        fence_generation,
                        attempt: 0,
                        retry_after: None,
                    });
                    self.phase = Phase::Recover;
                    return false;
                } else if response["fenced"] != false || response["drain_required"] != false {
                    return false;
                }
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
            Phase::Recover => {
                if response["ok"] != true
                    && let Some(recovery) = &mut self.recovery
                {
                    recovery.retry_after =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(1));
                }
                self.phase = Phase::Status;
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

fn valid_epoch(epoch: &str) -> bool {
    epoch.len() == 32
        && epoch
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_owner(owner: &str) -> bool {
    !owner.is_empty() && owner.len() <= 128 && !owner.chars().any(char::is_control)
}

pub(super) fn quiescent_fence(response: &Value) -> bool {
    response["ok"] == true
        && response["fenced"] == true
        && response["drain_required"] == false
        && response["startup_recovery_required"] == false
        && response["fence_generation"]
            .as_u64()
            .is_some_and(|generation| generation > 0)
        && response["in_flight"] == 0
        && response["bound_sources"] == 0
        && response["pending_source_retirements"] == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::tests::harness::{Harness, prepare, start};
    use amaterasu_contracts::{Command, Grants, Response, RuntimeConfig};
    use amaterasu_runtime::Broker;

    #[test]
    fn unchanged_consumer_uses_startup_selected_replacement_protocol_through_real_broker() {
        let mut host = Harness::new();
        let id = host.response(1, prepare())["session"].as_u64().unwrap();
        assert_eq!(host.response(2, start(id))["state"], "active");
        let target = LifecycleTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 2,
        };
        {
            let record = host.pipeline.records.get_mut(&id).unwrap();
            record.start.as_mut().unwrap().lifecycle_target = Some(target);
            record.lifecycle_input_token = Some("binding".into());
            record.client_haptics_requested = true;
            record.input_controller_supported = true;
            record.retain_control_retirement_epoch(&json!({"retirement_epoch":"c".repeat(32)}));
            record.feedback_source_observations.insert(3,json!({"source":{"version":1,"id":"a".repeat(32),"generation":2},"input_boot_epoch":"b".repeat(32),"device":{"slot":3,"generation":1,"capabilities":0},"scope_capabilities":[{"schema":"org.amaterasu.feedback.rumble","version":1,"direction":"to_client"}]}));
        }
        let mut registry = Registry::default();
        let mut admission = Admission::find(
            &host.pipeline.records,
            &registry,
            1,
            &[],
            "vendor.protocol/1",
        )
        .unwrap();
        let mut config = RuntimeConfig::default();
        config.principals.insert(
            "effect".into(),
            Grants {
                call: [effect_authority::SERVICE.into(), "vendor.protocol/1".into()].into(),
                ..Default::default()
            },
        );
        config.principals.insert(
            "pairing".into(),
            Grants {
                provide: [effect_authority::SERVICE.into()].into(),
                ..Default::default()
            },
        );
        config.principals.insert(
            "vendor".into(),
            Grants {
                provide: ["vendor.protocol/1".into()].into(),
                ..Default::default()
            },
        );
        let mut broker = Broker::new(config).unwrap();
        for (principal, service) in [
            ("pairing", effect_authority::SERVICE),
            ("vendor", "vendor.protocol/1"),
        ] {
            assert!(matches!(
                broker.dispatch(
                    principal,
                    Command::Register {
                        service: service.into()
                    }
                ),
                Response::Ok
            ));
        }
        let mut observed_vendor = false;
        let mut admitted = false;
        let mut status_count = 0;
        let mut recovered_owner = None;
        let mut recover_count = 0;
        for _ in 0..100 {
            if admission.advance(&host.pipeline.records, &mut registry, &mut |cmd| {
                Ok(broker.dispatch("effect", cmd))
            }) {
                admitted = true;
                break;
            }
            for provider in ["pairing", "vendor"] {
                let Response::Request {
                    id,
                    caller,
                    initiator,
                    payload,
                    ..
                } = broker.dispatch(provider, Command::Poll {})
                else {
                    continue;
                };
                assert_eq!(caller, "effect");
                assert_eq!(initiator, "effect");
                let result = match payload["action"].as_str().unwrap() {
                    "status" => {
                        status_count += 1;
                        if status_count == 1 {
                            json!({"ok":true,"epoch":"e".repeat(32),"owner_epoch":"owner","fence_generation":1,"fenced":true,"drain_required":false,"startup_recovery_required":false,"in_flight":0,"bound_sources":0,"pending_source_retirements":0})
                        } else {
                            json!({"ok":true,"epoch":"d".repeat(32),"owner_epoch":recovered_owner,"fence_generation":1,"fenced":false,"drain_required":false,"startup_recovery_required":false,"in_flight":0,"bound_sources":0,"pending_source_retirements":0})
                        }
                    }
                    "recover" => {
                        recover_count += 1;
                        assert_eq!(payload["epoch"], "e".repeat(32));
                        let owner = payload["owner_epoch"].as_str().unwrap();
                        assert!(valid_owner(owner));
                        recovered_owner = Some(owner.to_owned());
                        json!({"ok":true,"epoch":"d".repeat(32)})
                    }
                    "bind_feedback_source" => {
                        observed_vendor = provider == "vendor";
                        json!({"ok":true,"epoch":payload["authority_epoch"],"source":payload["source"],"scope":payload["scope"]})
                    }
                    "admit_stream_lease" => {
                        json!({"ok":true,"epoch":payload["epoch"],"operation_id":payload["operation_id"],"ticket":"f".repeat(32),"kind":"stream_lease","source":payload["source"],"scope":payload["scope"]})
                    }
                    other => panic!("unexpected action {other}"),
                };
                assert!(matches!(
                    broker.dispatch(
                        provider,
                        Command::Complete {
                            request: id,
                            payload: result
                        }
                    ),
                    Response::Ok
                ));
            }
        }
        assert!(observed_vendor && admitted);
        assert_eq!(recover_count, 1);
        assert!(status_count >= 2);
    }

    #[test]
    fn recovery_requires_a_fully_drained_non_startup_fence() {
        let base = json!({"ok":true,"epoch":"e".repeat(32),"owner_epoch":"owner",
            "fence_generation":1,"fenced":true,"drain_required":false,
            "startup_recovery_required":false,"in_flight":0,"bound_sources":0,
            "pending_source_retirements":0});
        assert!(quiescent_fence(&base));
        for (field, value) in [
            ("drain_required", json!(true)),
            ("startup_recovery_required", json!(true)),
            ("fence_generation", json!(0)),
            ("in_flight", json!(1)),
            ("bound_sources", json!(1)),
            ("pending_source_retirements", json!(1)),
        ] {
            let mut status = base.clone();
            status[field] = value;
            assert!(!quiescent_fence(&status), "{field}");
        }
    }
}
