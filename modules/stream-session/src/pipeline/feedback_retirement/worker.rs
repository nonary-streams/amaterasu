//! One whole-control-transport drain. It owns every local source on that
//! incarnation until all pairing tickets and source tombstones are settled.
use super::{
    Send,
    registry::{Binding, SourceRecord, Transport},
};
use amaterasu_module_sdk::{
    Command, Response,
    effect_authority::{self, StreamRetirement},
    effect_source_retirement::{self, State},
    nvhttp_source::BindFeedbackSource,
    transport::control_retirement::{Receipt, Request as ControlRequest},
};
use serde_json::Value;

pub(super) struct Call {
    command: Command,
    request: Option<u64>,
    result: Option<Value>,
    ever_sent: bool,
}
impl Call {
    pub(super) fn new(service: &str, payload: Value, operation_id: String) -> Self {
        Self {
            command: Command::Request {
                service: service.into(),
                payload,
                timeout_ms: 5000,
                operation_id,
                parent: None,
            },
            request: None,
            result: None,
            ever_sent: false,
        }
    }
    /// A lost reply never drops a handle or changes the operation identity.
    pub(super) fn poll(&mut self, send: &mut Send<'_>) -> Option<Value> {
        if let (Some(request), Some(payload)) = (self.request, self.result.as_ref()) {
            match send(Command::ReleaseResult { request }) {
                Ok(Response::Ok) => return Some(payload.clone()),
                Ok(Response::Error { code, .. }) if code == "not_found" => {
                    return Some(payload.clone());
                }
                _ => return None,
            }
        }
        if let Some(request) = self.request {
            match send(Command::TakeResult { request }) {
                Ok(Response::Result { payload }) => self.result = Some(payload),
                Ok(Response::Error { code, .. })
                    if code == "not_found" || code == "request_ended" =>
                {
                    self.request = None
                }
                _ => {}
            }
            return None;
        }
        self.ever_sent = true;
        if let Ok(Response::Accepted { id }) = send(self.command.clone()) {
            self.request = Some(id);
        }
        None
    }
    /// Reconcile an abandoned admission handle before dropping ownership.
    pub(super) fn settle(&mut self, send: &mut Send<'_>) -> bool {
        if self.request.is_none() {
            if !self.ever_sent {
                return true;
            }
            if let Ok(Response::Accepted { id }) = send(self.command.clone()) {
                self.request = Some(id);
            }
            return false;
        }
        let request = self.request.expect("checked handle");
        if self.result.is_none() {
            match send(Command::Cancel { request }) {
                Ok(Response::Ok) => {}
                Ok(Response::Error { code, .. })
                    if code == "not_found" || code == "request_ended" => {}
                _ => return false,
            }
        }
        match send(Command::ReleaseResult { request }) {
            Ok(Response::Ok) => true,
            Ok(Response::Error { code, .. }) if code == "not_found" => true,
            _ => false,
        }
    }
}
#[derive(Clone, Copy)]
enum Phase {
    LookupFence(usize),
    Fence(usize),
    RetireTransport,
    ResolveLease(usize),
    CompleteLease(usize),
    AckLease(usize),
    CompleteSource(usize),
    Done,
}

pub(super) struct Worker {
    binding: Binding,
    transport: Transport,
    records: Vec<SourceRecord>,
    fence_operations: Vec<Option<String>>,
    phase: Phase,
    call: Option<Call>,
    receipt: Option<StreamRetirement>,
}
impl Worker {
    pub fn new(binding: Binding, transport: Transport, records: Vec<SourceRecord>) -> Self {
        let count = records.len();
        Self {
            binding,
            transport,
            records,
            fence_operations: vec![None; count],
            phase: if count == 0 {
                Phase::RetireTransport
            } else {
                Phase::LookupFence(0)
            },
            call: None,
            receipt: None,
        }
    }
    fn pairing_call(
        &self,
        request: effect_source_retirement::Request,
        stage: &str,
        index: usize,
    ) -> Call {
        Call::new(
            effect_authority::SERVICE,
            serde_json::to_value(request).expect("serializable source operation"),
            format!("{}-{stage}-{index}", self.transport.retirement_operation()),
        )
    }
    fn lease_call(&self, request: effect_authority::Request, stage: &str, index: usize) -> Call {
        Call::new(
            effect_authority::SERVICE,
            serde_json::to_value(request).expect("serializable lease operation"),
            format!("{}-{stage}-{index}", self.transport.retirement_operation()),
        )
    }
    /// Returns a receipt only after all source and lease confirmations. Any
    /// malformed/uncertain response retains this worker and never acknowledges.
    pub fn advance(&mut self, send: &mut Send<'_>) -> Option<StreamRetirement> {
        let phase = self.phase;
        if matches!(phase, Phase::Done) {
            return self.receipt.clone();
        }
        if self.call.is_none() {
            let call = match phase {
                Phase::LookupFence(i) => {
                    let r = &self.records[i];
                    self.pairing_call(
                        effect_source_retirement::Request::LookupCurrentSourceRetirement {
                            epoch: r.epoch.clone(),
                            source: r.source.clone(),
                            scope: r.scope.clone(),
                        },
                        "lookup",
                        i,
                    )
                }
                Phase::Fence(i) => {
                    let r = &self.records[i];
                    let id = BindFeedbackSource {
                        authority_epoch: r.epoch.clone(),
                        source: r.source.clone(),
                        scope: r.scope.clone(),
                        input_boot_epoch: r.binding.input_boot_epoch.clone(),
                    }
                    .retirement_operation_id();
                    self.pairing_call(
                        effect_source_retirement::Request::FenceSource {
                            epoch: r.epoch.clone(),
                            source: r.source.clone(),
                            scope: r.scope.clone(),
                            operation_id: id,
                        },
                        "fence",
                        i,
                    )
                }
                Phase::RetireTransport => {
                    let operation = self.transport.retirement_operation();
                    let payload = serde_json::to_value(ControlRequest::Retire {
                        stream: self.transport.stream,
                        operation_id: &operation,
                        provider_epoch: &self.transport.epoch,
                    })
                    .ok()?;
                    Call::new(
                        &self.transport.service,
                        payload,
                        format!("{operation}-provider"),
                    )
                }
                Phase::ResolveLease(i) => {
                    let Some(request) = self.records[i].pending_lookup_request() else {
                        self.phase = if self.records[i].has_no_ticket() {
                            if i + 1 == self.records.len() {
                                Phase::CompleteSource(0)
                            } else {
                                Phase::ResolveLease(i + 1)
                            }
                        } else {
                            Phase::CompleteLease(i)
                        };
                        return None;
                    };
                    self.lease_call(request, "lease-lookup", i)
                }
                Phase::CompleteLease(i) => {
                    self.lease_call(self.records[i].completion_request().ok()?, "complete", i)
                }
                Phase::AckLease(i) => {
                    self.lease_call(self.records[i].acknowledgment_request().ok()?, "ack", i)
                }
                Phase::CompleteSource(i) => {
                    let r = &self.records[i];
                    let op = self.fence_operations[i].as_ref()?.clone();
                    self.pairing_call(
                        effect_source_retirement::Request::CompleteSourceRetirement {
                            epoch: r.epoch.clone(),
                            source: r.source.clone(),
                            scope: r.scope.clone(),
                            operation_id: op,
                            retirement: self.receipt.clone(),
                        },
                        "source",
                        i,
                    )
                }
                Phase::Done => unreachable!(),
            };
            self.call = Some(call);
        }
        let response = self.call.as_mut()?.poll(send)?;
        self.call = None;
        match phase {
            Phase::LookupFence(i) => {
                if response["ok"] == false && response["code"] == "unknown_source_retirement" {
                    self.phase = Phase::Fence(i);
                } else if let Ok(receipt) =
                    serde_json::from_value::<effect_source_retirement::Receipt>(response)
                {
                    let r = &self.records[i];
                    if receipt.matches_source(&r.epoch, &r.source, &r.scope)
                        && receipt.state == State::Fenced
                    {
                        self.fence_operations[i] = Some(receipt.operation_id);
                        self.phase = if i + 1 == self.records.len() {
                            Phase::RetireTransport
                        } else {
                            Phase::LookupFence(i + 1)
                        };
                    }
                }
            }
            Phase::Fence(i) => {
                if response["ok"] == false && response["code"] == "source_retirement_conflict" {
                    self.phase = Phase::LookupFence(i);
                } else if let Ok(receipt) =
                    serde_json::from_value::<effect_source_retirement::Receipt>(response)
                {
                    let r = &self.records[i];
                    let id = BindFeedbackSource {
                        authority_epoch: r.epoch.clone(),
                        source: r.source.clone(),
                        scope: r.scope.clone(),
                        input_boot_epoch: r.binding.input_boot_epoch.clone(),
                    }
                    .retirement_operation_id();
                    if receipt.matches(&r.epoch, &r.source, &r.scope, &id)
                        && receipt.state == State::Fenced
                    {
                        self.fence_operations[i] = Some(id);
                        self.phase = if i + 1 == self.records.len() {
                            Phase::RetireTransport
                        } else {
                            Phase::LookupFence(i + 1)
                        };
                    }
                }
            }
            Phase::RetireTransport => {
                if let Ok(receipt) = serde_json::from_value::<Receipt>(response)
                    && receipt.matches(
                        self.transport.stream,
                        &self.transport.retirement_operation(),
                        &self.transport.epoch,
                    )
                    && self
                        .records
                        .iter_mut()
                        .all(|record| record.retire_transport(&receipt).is_ok())
                {
                    self.receipt = Some(StreamRetirement {
                        ok: true,
                        service: self.transport.service.clone(),
                        stream: receipt.stream,
                        provider_epoch: receipt.provider_epoch,
                        operation_id: receipt.operation_id,
                        scope: receipt.scope,
                        state: receipt.state,
                    });
                    self.phase = if self.records.is_empty() {
                        Phase::Done
                    } else {
                        Phase::ResolveLease(0)
                    };
                }
            }
            Phase::ResolveLease(i) => {
                if self.records[i].resolve_pending(&response).is_ok() {
                    self.phase = if self.records[i].has_no_ticket() {
                        if i + 1 == self.records.len() {
                            Phase::CompleteSource(0)
                        } else {
                            Phase::ResolveLease(i + 1)
                        }
                    } else {
                        Phase::CompleteLease(i)
                    };
                }
            }
            Phase::CompleteLease(i) => {
                if self.records[i].confirm_completion(&response).is_ok() {
                    self.phase = Phase::AckLease(i);
                }
            }
            Phase::AckLease(i) => {
                if self.records[i].confirm_acknowledgment(&response).is_ok() {
                    self.phase = if i + 1 == self.records.len() {
                        Phase::CompleteSource(0)
                    } else {
                        Phase::ResolveLease(i + 1)
                    };
                }
            }
            Phase::CompleteSource(i) => {
                if let Ok(receipt) =
                    serde_json::from_value::<effect_source_retirement::Receipt>(response)
                {
                    let r = &self.records[i];
                    if receipt.matches(
                        &r.epoch,
                        &r.source,
                        &r.scope,
                        self.fence_operations[i].as_deref()?,
                    ) && receipt.state == State::Retired
                    {
                        self.phase = if i + 1 == self.records.len() {
                            Phase::Done
                        } else {
                            Phase::CompleteSource(i + 1)
                        };
                    }
                }
            }
            Phase::Done => unreachable!(),
        }
        if matches!(self.phase, Phase::Done) {
            self.receipt.clone()
        } else {
            None
        }
    }
    pub fn watch_context(&self) -> Option<(String, String, Binding, Transport)> {
        self.records.first().map(|record| {
            (
                record.epoch.clone(),
                record.source.effect_owner_epoch.clone(),
                record.binding.clone(),
                self.transport.clone(),
            )
        })
    }
    pub fn bindings(&self) -> Vec<Binding> {
        let mut bindings = vec![self.binding.clone()];
        for record in &self.records {
            if !bindings.contains(&record.binding) {
                bindings.push(record.binding.clone());
            }
        }
        bindings
    }
    pub fn transport(&self) -> &Transport {
        &self.transport
    }
    pub fn source_count(&self) -> usize {
        self.records.len()
    }
}

#[cfg(test)]
mod tests {
    use super::super::registry::Registry;
    use super::*;
    use amaterasu_extension_api::LifecycleTarget;
    use amaterasu_module_sdk::effect_authority::{
        FeedbackCapability, FeedbackDirection, Source, StreamScope,
    };
    use serde_json::json;
    use std::collections::BTreeMap;
    fn source(slot: u16) -> Source {
        Source {
            version: 1,
            id: "a".repeat(32),
            generation: 2,
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            stream_generation: 2,
            device: format!("device-{slot}"),
            device_generation: 1,
            effect_owner_epoch: "owner".into(),
        }
    }
    fn scope(slot: u16) -> StreamScope {
        StreamScope {
            capabilities: [FeedbackCapability {
                schema: "org.amaterasu.feedback.rumble".into(),
                version: 1,
                direction: FeedbackDirection::ToClient,
            }]
            .into(),
            controller_slot: slot,
            transport_service: "control/1".into(),
            transport_stream: 7,
            transport_epoch: "c".repeat(32),
        }
    }
    fn binding() -> Binding {
        Binding {
            token: "binding".into(),
            target: LifecycleTarget::Stream {
                host_epoch: "host".into(),
                session_id: "session".into(),
                stream_id: "stream".into(),
                generation: 2,
            },
            input_boot_epoch: "b".repeat(32),
        }
    }
    fn apply_pairing(
        gate: &mut super::actual_pairing_ledger::EffectGate,
        payload: &Value,
    ) -> Result<Value, &'static str> {
        let epoch = gate.epoch().to_owned();
        match payload["action"].as_str().unwrap() {
            "status" => Ok(json!({"ok":true,"epoch":epoch,"owner_epoch":gate.owner_epoch(),"startup_recovery_required":gate.startup_recovery_required(),"fence_generation":gate.fence_generation(),"fenced":gate.fenced(),"drain_required":gate.drain_required(),"pending_source_retirements":gate.pending_source_retirements()})),
            "drain" => gate.acknowledge_drain(&epoch,payload["fence_generation"].as_u64().unwrap()).map(|()|json!({"ok":true,"epoch":epoch,"fence_generation":payload["fence_generation"]})),
            "lookup_current_source_retirement" => gate.lookup_current_source_retirement(&epoch,&serde_json::from_value(payload["source"].clone()).unwrap(),&serde_json::from_value(payload["scope"].clone()).unwrap()).map(|(op,done)|json!({"ok":true,"epoch":epoch,"source":payload["source"],"scope":payload["scope"],"operation_id":op,"state":if done {"retired"} else {"fenced"}})),
            "fence_source" => gate.fence_source(&epoch,&serde_json::from_value(payload["source"].clone()).unwrap(),&serde_json::from_value(payload["scope"].clone()).unwrap(),payload["operation_id"].as_str().unwrap()).map(|done|json!({"ok":true,"epoch":epoch,"source":payload["source"],"scope":payload["scope"],"operation_id":payload["operation_id"],"state":if done {"retired"} else {"fenced"}})),
            "lookup_stream_lease" => gate.lookup_stream_lease(&serde_json::from_value(payload["source"].clone()).unwrap(),payload["operation_id"].as_str().unwrap(),&serde_json::from_value(payload["scope"].clone()).unwrap()).map(|(ticket,fenced)|json!({"ok":true,"epoch":epoch,"operation_id":payload["operation_id"],"ticket":ticket,"fenced":fenced,"kind":"stream_lease","source":payload["source"],"scope":payload["scope"]})),
            "complete_stream_lease" => gate.complete_stream_lease(&epoch,payload["operation_id"].as_str().unwrap(),payload["ticket"].as_str().unwrap(),&serde_json::from_value(payload["retirement"].clone()).unwrap()).map(|()|json!({"ok":true,"epoch":epoch,"operation_id":payload["operation_id"]})),
            "acknowledge" => gate.acknowledge(&epoch,payload["operation_id"].as_str().unwrap(),payload["ticket"].as_str().unwrap()).map(|()|json!({"ok":true,"epoch":epoch,"operation_id":payload["operation_id"]})),
            "complete_source_retirement" => gate.complete_source_retirement(&epoch,&serde_json::from_value(payload["source"].clone()).unwrap(),&serde_json::from_value(payload["scope"].clone()).unwrap(),payload["operation_id"].as_str().unwrap(),serde_json::from_value::<Option<StreamRetirement>>(payload["retirement"].clone()).unwrap().as_ref()).map(|()|json!({"ok":true,"epoch":epoch,"source":payload["source"],"scope":payload["scope"],"operation_id":payload["operation_id"],"state":"retired"})),
            _ => panic!("unexpected pairing action"),
        }
    }
    #[derive(Default)]
    struct BrokerFixture {
        next: u64,
        calls: BTreeMap<u64, (String, Value)>,
        fences: usize,
        completed: usize,
        acked: usize,
        sources: usize,
        retired: bool,
        lost_release: bool,
        wrong_epoch: bool,
    }
    impl BrokerFixture {
        fn send(&mut self, command: Command) -> Result<Response, String> {
            match command {
                Command::Request {
                    service, payload, ..
                } => {
                    self.next += 1;
                    self.calls.insert(self.next, (service, payload));
                    Ok(Response::Accepted { id: self.next })
                }
                Command::TakeResult { request } => {
                    let Some((service, payload)) = self.calls.get(&request).cloned() else {
                        return Ok(Response::Error {
                            code: "not_found".into(),
                            message: String::new(),
                        });
                    };
                    let action = payload["action"].as_str().unwrap();
                    let result = match action {
                        "lookup_current_source_retirement" => {
                            json!({"ok":false,"code":"unknown_source_retirement"})
                        }
                        "fence_source" => {
                            self.fences += 1;
                            json!({"ok":true,"epoch":payload["epoch"],"source":payload["source"],"scope":payload["scope"],"operation_id":payload["operation_id"],"state":"fenced"})
                        }
                        "retire" => {
                            assert_eq!(service, "control/1");
                            assert_eq!(self.fences, 2);
                            self.retired = true;
                            json!({"ok":true,"state":"retired","scope":"whole_control_stream","stream":payload["stream"],"operation_id":payload["operation_id"],"provider_epoch":if self.wrong_epoch {json!("d".repeat(32))} else {payload["provider_epoch"].clone()}})
                        }
                        "lookup_stream_lease" => {
                            assert!(self.retired);
                            json!({"ok":false,"code":"unknown_admission"})
                        }
                        "complete_stream_lease" => {
                            assert!(self.retired);
                            self.completed += 1;
                            json!({"ok":true,"epoch":payload["epoch"],"operation_id":payload["operation_id"]})
                        }
                        "acknowledge" => {
                            self.acked += 1;
                            json!({"ok":true,"epoch":payload["epoch"],"operation_id":payload["operation_id"]})
                        }
                        "complete_source_retirement" => {
                            assert_eq!(self.completed, 1);
                            assert_eq!(self.acked, 1);
                            self.sources += 1;
                            json!({"ok":true,"epoch":payload["epoch"],"source":payload["source"],"scope":payload["scope"],"operation_id":payload["operation_id"],"state":"retired"})
                        }
                        _ => panic!("unexpected action {action}"),
                    };
                    Ok(Response::Result { payload: result })
                }
                Command::ReleaseResult { request } => {
                    if !self.lost_release {
                        self.lost_release = true;
                        self.calls.remove(&request);
                        return Err("release reply lost".into());
                    }
                    if self.calls.remove(&request).is_some() {
                        Ok(Response::Ok)
                    } else {
                        Ok(Response::Error {
                            code: "not_found".into(),
                            message: String::new(),
                        })
                    }
                }
                _ => panic!("unexpected broker call"),
            }
        }
    }
    #[test]
    fn real_pairing_ledger_and_broker_settle_shared_sources_with_synthetic_control_receipt() {
        use super::actual_pairing_ledger::EffectGate;
        use amaterasu_contracts::{Grants, RuntimeConfig};
        use amaterasu_runtime::Broker;
        let mut gate = EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let epoch = gate.epoch().to_owned();
        let mut registry = Registry::default();
        for slot in [1, 2] {
            gate.bind_scoped(source(slot), Some(scope(slot))).unwrap();
            registry
                .reserve(
                    epoch.clone(),
                    source(slot),
                    scope(slot),
                    binding(),
                    format!("lease-{slot}"),
                    u64::from(slot),
                )
                .unwrap();
        }
        let ticket = gate
            .admit_stream_lease(&source(1), "lease-1", 1, scope(1))
            .unwrap()
            .to_owned();
        registry.accept_admission(&source(1),&json!({"ok":true,"epoch":epoch,"operation_id":"lease-1","ticket":ticket,"kind":"stream_lease","source":source(1),"scope":scope(1)})).unwrap();
        let (transport, records) = registry.take_transport(&binding()).unwrap();
        let mut worker = Worker::new(binding(), transport, records);
        let mut config = RuntimeConfig::default();
        config.principals.insert(
            "effect".into(),
            Grants {
                call: [effect_authority::SERVICE.into(), "control/1".into()].into(),
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
            "control".into(),
            Grants {
                provide: ["control/1".into()].into(),
                ..Default::default()
            },
        );
        let mut broker = Broker::new(config).unwrap();
        for (principal, service) in [
            ("pairing", effect_authority::SERVICE),
            ("control", "control/1"),
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
        let mut synthetic_receipts = 0;
        let mut events = Vec::new();
        let mut lost_once = false;
        let mut result = None;
        for _ in 0..1500 {
            if let Some(receipt) = worker.advance(&mut |cmd| {
                let is_take = matches!(cmd, Command::TakeResult { .. });
                let reply = broker.dispatch("effect", cmd);
                if is_take && matches!(reply, Response::Result { .. }) && !lost_once {
                    lost_once = true;
                    Err("reply lost".into())
                } else {
                    Ok(reply)
                }
            }) {
                result = Some(receipt);
                break;
            }
            for provider in ["pairing", "control"] {
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
                let action = payload["action"].as_str().unwrap();
                let result: Result<Value, &str> = if action == "retire" {
                    synthetic_receipts += 1;
                    Ok(
                        json!({"ok":true,"state":"retired","scope":"whole_control_stream","stream":payload["stream"],"operation_id":payload["operation_id"],"provider_epoch":payload["provider_epoch"]}),
                    )
                } else {
                    apply_pairing(&mut gate, &payload)
                };
                let response = result.unwrap_or_else(|code| json!({"ok":false,"code":code}));
                events.push(json!({"action":action,"response":response}));
                assert!(matches!(
                    broker.dispatch(
                        provider,
                        Command::Complete {
                            request: id,
                            payload: response
                        }
                    ),
                    Response::Ok
                ));
            }
        }
        assert!(result.is_some(), "events: {events:?}");
        assert!(lost_once);
        assert_eq!(synthetic_receipts, 1);
        assert_eq!(gate.in_flight(), 0);
        assert_eq!(gate.pending_source_retirements(), 0);
        assert!(gate.bind_scoped(source(1), Some(scope(1))).is_err());
    }

    #[test]
    fn real_pairing_ledger_and_control_provider_retire_queued_enet_through_broker() {
        use super::actual_pairing_ledger::EffectGate;
        use amaterasu_contracts::{Grants, RuntimeConfig};
        use amaterasu_runtime::Broker;
        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver.set_nonblocking(true).unwrap();
        let mut control = amaterasu_control_transport::test_support::ControlFixture::new("effect");
        let opened = control.open("effect").unwrap();
        let stream = opened["stream"].as_u64().unwrap();
        let transport_epoch = opened["retirement_epoch"].as_str().unwrap().to_owned();
        control
            .stage_feedback("effect", stream, receiver.local_addr().unwrap())
            .unwrap();
        let selected_scope = |slot| {
            let mut selected = scope(slot);
            selected.transport_service = "amaterasu.control-transport/1".into();
            selected.transport_stream = stream;
            selected.transport_epoch = transport_epoch.clone();
            selected
        };
        let mut gate = EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let epoch = gate.epoch().to_owned();
        let mut registry = Registry::default();
        for slot in [1, 2] {
            gate.bind_scoped(source(slot), Some(selected_scope(slot)))
                .unwrap();
            registry
                .reserve(
                    epoch.clone(),
                    source(slot),
                    selected_scope(slot),
                    binding(),
                    format!("lease-{slot}"),
                    u64::from(slot),
                )
                .unwrap();
        }
        let ticket = gate
            .admit_stream_lease(&source(1), "lease-1", 1, selected_scope(1))
            .unwrap()
            .to_owned();
        registry.accept_admission(&source(1),&json!({"ok":true,"epoch":epoch,"operation_id":"lease-1","ticket":ticket,"kind":"stream_lease","source":source(1),"scope":selected_scope(1)})).unwrap();
        let (transport, records) = registry.take_transport(&binding()).unwrap();
        let mut worker = Worker::new(binding(), transport, records);
        let mut config = RuntimeConfig::default();
        config.principals.insert(
            "effect".into(),
            Grants {
                call: [
                    effect_authority::SERVICE.into(),
                    "amaterasu.control-transport/1".into(),
                ]
                .into(),
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
            "control".into(),
            Grants {
                provide: ["amaterasu.control-transport/1".into()].into(),
                ..Default::default()
            },
        );
        let mut broker = Broker::new(config).unwrap();
        for (principal, service) in [
            ("pairing", effect_authority::SERVICE),
            ("control", "amaterasu.control-transport/1"),
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
        let mut provider_receipts = 0;
        let mut events = Vec::new();
        let mut lost_once = false;
        let mut result = None;
        for _ in 0..1500 {
            if let Some(receipt) = worker.advance(&mut |cmd| {
                let is_take = matches!(cmd, Command::TakeResult { .. });
                let reply = broker.dispatch("effect", cmd);
                if is_take && matches!(reply, Response::Result { .. }) && !lost_once {
                    lost_once = true;
                    Err("reply lost".into())
                } else {
                    Ok(reply)
                }
            }) {
                result = Some(receipt);
                break;
            }
            for provider in ["pairing", "control"] {
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
                let action = payload["action"].as_str().unwrap();
                let result: Result<Value, &str> = if action == "retire" {
                    provider_receipts += 1;
                    control.handle("effect", payload.clone())
                } else {
                    apply_pairing(&mut gate, &payload)
                };
                let response = result.unwrap_or_else(|code| json!({"ok":false,"code":code}));
                events.push(json!({"action":action,"response":response}));
                assert!(matches!(
                    broker.dispatch(
                        provider,
                        Command::Complete {
                            request: id,
                            payload: response
                        }
                    ),
                    Response::Ok
                ));
            }
        }
        assert!(result.is_some(), "events: {events:?}");
        assert!(lost_once);
        assert_eq!(provider_receipts, 1);
        control.tick();
        let mut buffer = [0u8; 64];
        assert_eq!(
            receiver.recv_from(&mut buffer).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(gate.in_flight(), 0);
        assert_eq!(gate.pending_source_retirements(), 0);
        assert!(
            gate.bind_scoped(source(1), Some(selected_scope(1)))
                .is_err()
        );
    }

    #[test]
    fn real_broker_input_ack_follows_pairing_and_control_retirement() {
        use super::actual_pairing_ledger::EffectGate;
        use amaterasu_contracts::{Grants, RuntimeConfig};
        use amaterasu_runtime::Broker;
        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver.set_nonblocking(true).unwrap();
        let mut control = amaterasu_control_transport::test_support::ControlFixture::new("effect");
        let opened = control.open("effect").unwrap();
        let stream = opened["stream"].as_u64().unwrap();
        let transport_epoch = opened["retirement_epoch"].as_str().unwrap().to_owned();
        control
            .stage_feedback("effect", stream, receiver.local_addr().unwrap())
            .unwrap();
        let selected_scope = |slot| {
            let mut selected = scope(slot);
            selected.transport_service = "amaterasu.control-transport/1".into();
            selected.transport_stream = stream;
            selected.transport_epoch = transport_epoch.clone();
            selected
        };
        let mut gate = EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let epoch = gate.epoch().to_owned();
        let mut registry = Registry::default();
        for slot in [1, 2] {
            gate.bind_scoped(source(slot), Some(selected_scope(slot)))
                .unwrap();
            registry
                .reserve(
                    epoch.clone(),
                    source(slot),
                    selected_scope(slot),
                    binding(),
                    format!("lease-{slot}"),
                    u64::from(slot),
                )
                .unwrap();
        }
        let ticket = gate
            .admit_stream_lease(&source(1), "lease-1", 1, selected_scope(1))
            .unwrap()
            .to_owned();
        registry.accept_admission(&source(1),&json!({"ok":true,"epoch":epoch,"operation_id":"lease-1","ticket":ticket,"kind":"stream_lease","source":source(1),"scope":selected_scope(1)})).unwrap();
        use crate::pipeline::{
            record::Resource,
            tests::harness::{Harness, prepare, start},
        };
        let mut host = Harness::new();
        let session = host.response(1, prepare())["session"].as_u64().unwrap();
        assert_eq!(host.response(2, start(session))["state"], "active");
        {
            let record = host.pipeline.records.get_mut(&session).unwrap();
            record.lifecycle_input_token = Some("binding".into());
            record.start.as_mut().unwrap().lifecycle_target = Some(binding().target);
            record.resources.insert(Resource::Control, stream);
            record.retain_control_retirement_epoch(&json!({"retirement_epoch":transport_epoch}));
        }
        let mut inbox = super::super::Inbox {
            registry,
            ..Default::default()
        };
        let mut config = RuntimeConfig::default();
        config.principals.insert(
            "input".into(),
            Grants {
                call: ["effect/1".into()].into(),
                ..Default::default()
            },
        );
        config.principals.insert(
            "effect".into(),
            Grants {
                provide: ["effect/1".into()].into(),
                call: [
                    effect_authority::SERVICE.into(),
                    "amaterasu.control-transport/1".into(),
                ]
                .into(),
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
            "control".into(),
            Grants {
                provide: ["amaterasu.control-transport/1".into()].into(),
                ..Default::default()
            },
        );
        let mut broker = Broker::new(config).unwrap();
        for (principal, service) in [
            ("pairing", effect_authority::SERVICE),
            ("control", "amaterasu.control-transport/1"),
            ("effect", "effect/1"),
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
        let request_payload = json!({"action":"retire_input_feedback","version":1,"operation_id":"input-retire","token":"binding","target":binding().target,"input_boot_epoch":"b".repeat(32)});
        let Response::Accepted { id: input_request } = broker.dispatch(
            "input",
            Command::Request {
                service: "effect/1".into(),
                payload: request_payload,
                timeout_ms: 5000,
                operation_id: "input-retire".into(),
                parent: None,
            },
        ) else {
            panic!("input admission")
        };
        let Response::Request {
            caller,
            initiator,
            payload,
            ..
        } = broker.dispatch("effect", Command::Poll {})
        else {
            panic!("input delivery")
        };
        inbox.receive(
            input_request,
            &caller,
            &initiator,
            Some("input"),
            payload,
            &mut |cmd| Ok(broker.dispatch("effect", cmd)),
        );
        assert!(matches!(
            broker.dispatch(
                "input",
                Command::TakeResult {
                    request: input_request
                }
            ),
            Response::Pending
        ));
        let mut provider_receipts = 0;
        let mut events = Vec::new();
        let mut lost_once = false;
        let mut result = None;
        for _ in 0..1500 {
            inbox.prune_unstarted(&mut |cmd| Ok(broker.dispatch("effect", cmd)));
            inbox.advance(&mut host.pipeline.records, false, &mut |cmd| {
                let is_take = matches!(cmd, Command::TakeResult { .. });
                let reply = broker.dispatch("effect", cmd);
                if is_take && matches!(reply, Response::Result { .. }) && !lost_once {
                    lost_once = true;
                    Err("reply lost".into())
                } else {
                    Ok(reply)
                }
            });
            for provider in ["pairing", "control"] {
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
                let action = payload["action"].as_str().unwrap();
                let result: Result<Value, &str> = if action == "retire" {
                    provider_receipts += 1;
                    control.handle("effect", payload.clone())
                } else {
                    apply_pairing(&mut gate, &payload)
                };
                let response = result.unwrap_or_else(|code| json!({"ok":false,"code":code}));
                events.push(json!({"action":action,"response":response}));
                assert!(matches!(
                    broker.dispatch(
                        provider,
                        Command::Complete {
                            request: id,
                            payload: response
                        }
                    ),
                    Response::Ok
                ));
            }
            if let Response::Result { payload } = broker.dispatch(
                "input",
                Command::TakeResult {
                    request: input_request,
                },
            ) {
                result = Some(payload);
                break;
            }
        }
        let result = result.unwrap_or_else(|| panic!("events: {events:?}"));
        assert_eq!(result["source_count"], 2);
        assert_eq!(result["pairing_retired"], true);
        assert_eq!(result["retirement"]["stream"], stream);
        assert!(lost_once);
        assert_eq!(provider_receipts, 1);
        control.tick();
        let mut buffer = [0u8; 64];
        assert_eq!(
            receiver.recv_from(&mut buffer).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(gate.in_flight(), 0);
        assert_eq!(gate.pending_source_retirements(), 0);
        assert!(
            gate.bind_scoped(source(1), Some(selected_scope(1)))
                .is_err()
        );
        assert!(
            gate.admit_stream_lease(&source(1), "lease-1", 3, selected_scope(1))
                .is_err()
        );
        let mut replacement = source(1);
        replacement.device_generation = 2;
        assert!(
            gate.bind_scoped(replacement.clone(), Some(selected_scope(1)))
                .is_err()
        );
        let mut other_transport = selected_scope(1);
        other_transport.transport_stream = stream.checked_add(1).unwrap();
        assert!(gate.bind_scoped(replacement, Some(other_transport)).is_ok());
    }

    #[test]
    fn cancelled_input_retry_recovers_real_transport_retirement_receipt() {
        use super::actual_pairing_ledger::EffectGate;
        use amaterasu_contracts::{Grants, RuntimeConfig};
        use amaterasu_runtime::Broker;
        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver.set_nonblocking(true).unwrap();
        let mut control = amaterasu_control_transport::test_support::ControlFixture::new("effect");
        let opened = control.open("effect").unwrap();
        let stream = opened["stream"].as_u64().unwrap();
        let transport_epoch = opened["retirement_epoch"].as_str().unwrap().to_owned();
        control
            .stage_feedback("effect", stream, receiver.local_addr().unwrap())
            .unwrap();
        let selected_scope = |slot| {
            let mut selected = scope(slot);
            selected.transport_service = "amaterasu.control-transport/1".into();
            selected.transport_stream = stream;
            selected.transport_epoch = transport_epoch.clone();
            selected
        };
        let mut gate = EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let epoch = gate.epoch().to_owned();
        let mut registry = Registry::default();
        for slot in [1, 2] {
            gate.bind_scoped(source(slot), Some(selected_scope(slot)))
                .unwrap();
            registry
                .reserve(
                    epoch.clone(),
                    source(slot),
                    selected_scope(slot),
                    binding(),
                    format!("lease-{slot}"),
                    u64::from(slot),
                )
                .unwrap();
        }
        let ticket = gate
            .admit_stream_lease(&source(1), "lease-1", 1, selected_scope(1))
            .unwrap()
            .to_owned();
        registry.accept_admission(&source(1),&json!({"ok":true,"epoch":epoch,"operation_id":"lease-1","ticket":ticket,"kind":"stream_lease","source":source(1),"scope":selected_scope(1)})).unwrap();
        use crate::pipeline::{
            record::Resource,
            tests::harness::{Harness, prepare, start},
        };
        let mut host = Harness::new();
        let session = host.response(1, prepare())["session"].as_u64().unwrap();
        assert_eq!(host.response(2, start(session))["state"], "active");
        {
            let record = host.pipeline.records.get_mut(&session).unwrap();
            record.lifecycle_input_token = Some("binding".into());
            record.start.as_mut().unwrap().lifecycle_target = Some(binding().target);
            record.resources.insert(Resource::Control, stream);
            record.retain_control_retirement_epoch(&json!({"retirement_epoch":transport_epoch}));
        }
        let mut inbox = super::super::Inbox {
            registry,
            ..Default::default()
        };
        let mut config = RuntimeConfig::default();
        config.principals.insert(
            "input".into(),
            Grants {
                call: ["effect/1".into()].into(),
                ..Default::default()
            },
        );
        config.principals.insert(
            "effect".into(),
            Grants {
                provide: ["effect/1".into()].into(),
                call: [
                    effect_authority::SERVICE.into(),
                    "amaterasu.control-transport/1".into(),
                ]
                .into(),
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
            "control".into(),
            Grants {
                provide: ["amaterasu.control-transport/1".into()].into(),
                ..Default::default()
            },
        );
        let mut broker = Broker::new(config).unwrap();
        for (principal, service) in [
            ("pairing", effect_authority::SERVICE),
            ("control", "amaterasu.control-transport/1"),
            ("effect", "effect/1"),
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
        let request_payload = json!({"action":"retire_input_feedback","version":1,"operation_id":"input-retire","token":"binding","target":binding().target,"input_boot_epoch":"b".repeat(32)});
        let Response::Accepted { id: input_request } = broker.dispatch(
            "input",
            Command::Request {
                service: "effect/1".into(),
                payload: request_payload.clone(),
                timeout_ms: 5000,
                operation_id: "input-retire".into(),
                parent: None,
            },
        ) else {
            panic!("input admission")
        };
        let Response::Request {
            caller,
            initiator,
            payload,
            ..
        } = broker.dispatch("effect", Command::Poll {})
        else {
            panic!("input delivery")
        };
        inbox.receive(
            input_request,
            &caller,
            &initiator,
            Some("input"),
            payload,
            &mut |cmd| Ok(broker.dispatch("effect", cmd)),
        );
        assert!(matches!(
            broker.dispatch(
                "input",
                Command::TakeResult {
                    request: input_request
                }
            ),
            Response::Pending
        ));
        let mut provider_receipts = 0;
        let mut events = Vec::new();
        let mut lost_once = false;
        let mut result = None;
        let mut canceled = false;
        let mut retried = false;
        let mut active_request = input_request;
        for _ in 0..1500 {
            inbox.prune_unstarted(&mut |cmd| Ok(broker.dispatch("effect", cmd)));
            inbox.advance(&mut host.pipeline.records, false, &mut |cmd| {
                let is_take = matches!(cmd, Command::TakeResult { .. });
                let reply = broker.dispatch("effect", cmd);
                if is_take && matches!(reply, Response::Result { .. }) && !lost_once {
                    lost_once = true;
                    Err("reply lost".into())
                } else {
                    Ok(reply)
                }
            });
            for provider in ["pairing", "control"] {
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
                let action = payload["action"].as_str().unwrap();
                let result: Result<Value, &str> = if action == "retire" {
                    provider_receipts += 1;
                    control.handle("effect", payload.clone())
                } else {
                    apply_pairing(&mut gate, &payload)
                };
                let response = result.unwrap_or_else(|code| json!({"ok":false,"code":code}));
                events.push(json!({"action":action,"response":response}));
                assert!(matches!(
                    broker.dispatch(
                        provider,
                        Command::Complete {
                            request: id,
                            payload: response
                        }
                    ),
                    Response::Ok
                ));
            }
            if !canceled && gate.pending_source_retirements() > 0 {
                assert!(matches!(
                    broker.dispatch(
                        "input",
                        Command::Cancel {
                            request: input_request
                        }
                    ),
                    Response::Ok
                ));
                assert!(matches!(
                    broker.dispatch(
                        "input",
                        Command::ReleaseResult {
                            request: input_request
                        }
                    ),
                    Response::Ok
                ));
                canceled = true;
            }
            if canceled
                && !retried
                && inbox.entries.is_empty()
                && inbox.completed.contains_key("input-retire")
            {
                let Response::Accepted { id } = broker.dispatch(
                    "input",
                    Command::Request {
                        service: "effect/1".into(),
                        payload: request_payload.clone(),
                        timeout_ms: 5000,
                        operation_id: "input-retire-retry".into(),
                        parent: None,
                    },
                ) else {
                    panic!("retry admission")
                };
                let Response::Request {
                    caller,
                    initiator,
                    payload,
                    ..
                } = broker.dispatch("effect", Command::Poll {})
                else {
                    panic!("retry delivery")
                };
                inbox.receive(
                    id,
                    &caller,
                    &initiator,
                    Some("input"),
                    payload,
                    &mut |cmd| Ok(broker.dispatch("effect", cmd)),
                );
                active_request = id;
                retried = true;
            }
            if retried
                && let Response::Result { payload } = broker.dispatch(
                    "input",
                    Command::TakeResult {
                        request: active_request,
                    },
                )
            {
                result = Some(payload);
                break;
            }
        }
        let result = result.unwrap_or_else(|| panic!("events: {events:?}"));
        assert!(canceled && retried);
        assert_eq!(result["source_count"], 2);
        assert_eq!(result["pairing_retired"], true);
        assert_eq!(result["retirement"]["stream"], stream);
        assert!(lost_once);
        assert_eq!(provider_receipts, 1);
        control.tick();
        let mut buffer = [0u8; 64];
        assert_eq!(
            receiver.recv_from(&mut buffer).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(gate.in_flight(), 0);
        assert_eq!(gate.pending_source_retirements(), 0);
        assert!(
            gate.bind_scoped(source(1), Some(selected_scope(1)))
                .is_err()
        );
    }

    #[test]
    fn global_permission_fence_waits_for_real_control_retirement_and_all_leases() {
        use super::actual_pairing_ledger::EffectGate;
        use amaterasu_contracts::{Grants, RuntimeConfig};
        use amaterasu_runtime::Broker;
        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver.set_nonblocking(true).unwrap();
        let mut control = amaterasu_control_transport::test_support::ControlFixture::new("effect");
        let opened = control.open("effect").unwrap();
        let stream = opened["stream"].as_u64().unwrap();
        let transport_epoch = opened["retirement_epoch"].as_str().unwrap().to_owned();
        control
            .stage_feedback("effect", stream, receiver.local_addr().unwrap())
            .unwrap();
        let selected_scope = |slot| {
            let mut selected = scope(slot);
            selected.transport_service = "amaterasu.control-transport/1".into();
            selected.transport_stream = stream;
            selected.transport_epoch = transport_epoch.clone();
            selected
        };
        let mut gate = EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let epoch = gate.epoch().to_owned();
        let mut registry = Registry::default();
        for slot in [1, 2] {
            gate.bind_scoped(source(slot), Some(selected_scope(slot)))
                .unwrap();
            registry
                .reserve(
                    epoch.clone(),
                    source(slot),
                    selected_scope(slot),
                    binding(),
                    format!("lease-{slot}"),
                    u64::from(slot),
                )
                .unwrap();
        }
        let ticket = gate
            .admit_stream_lease(&source(1), "lease-1", 1, selected_scope(1))
            .unwrap()
            .to_owned();
        registry.accept_admission(&source(1),&json!({"ok":true,"epoch":epoch,"operation_id":"lease-1","ticket":ticket,"kind":"stream_lease","source":source(1),"scope":selected_scope(1)})).unwrap();
        use crate::pipeline::{
            record::Resource,
            tests::harness::{Harness, prepare, start},
        };
        let mut host = Harness::new();
        let session = host.response(1, prepare())["session"].as_u64().unwrap();
        assert_eq!(host.response(2, start(session))["state"], "active");
        {
            let record = host.pipeline.records.get_mut(&session).unwrap();
            record.lifecycle_input_token = Some("binding".into());
            record.start.as_mut().unwrap().lifecycle_target = Some(binding().target);
            record.resources.insert(Resource::Control, stream);
            record.retain_control_retirement_epoch(&json!({"retirement_epoch":transport_epoch}));
        }
        gate.fence();
        assert!(gate.drain_required());
        assert!(gate.recover("replacement-owner").is_err());
        let mut inbox = super::super::Inbox {
            registry,
            ..Default::default()
        };
        let mut config = RuntimeConfig::default();
        config.principals.insert(
            "input".into(),
            Grants {
                call: ["effect/1".into()].into(),
                ..Default::default()
            },
        );
        config.principals.insert(
            "effect".into(),
            Grants {
                provide: ["effect/1".into()].into(),
                call: [
                    effect_authority::SERVICE.into(),
                    "amaterasu.control-transport/1".into(),
                ]
                .into(),
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
            "control".into(),
            Grants {
                provide: ["amaterasu.control-transport/1".into()].into(),
                ..Default::default()
            },
        );
        let mut broker = Broker::new(config).unwrap();
        for (principal, service) in [
            ("pairing", effect_authority::SERVICE),
            ("control", "amaterasu.control-transport/1"),
            ("effect", "effect/1"),
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
        let request_payload = json!({"action":"retire_input_feedback","version":1,"operation_id":"input-retire","token":"binding","target":binding().target,"input_boot_epoch":"b".repeat(32)});
        let Response::Accepted { id: input_request } = broker.dispatch(
            "input",
            Command::Request {
                service: "effect/1".into(),
                payload: request_payload,
                timeout_ms: 5000,
                operation_id: "input-retire".into(),
                parent: None,
            },
        ) else {
            panic!("input admission")
        };
        let Response::Request {
            caller,
            initiator,
            payload,
            ..
        } = broker.dispatch("effect", Command::Poll {})
        else {
            panic!("input delivery")
        };
        inbox.receive(
            input_request,
            &caller,
            &initiator,
            Some("input"),
            payload,
            &mut |cmd| Ok(broker.dispatch("effect", cmd)),
        );
        assert!(matches!(
            broker.dispatch(
                "input",
                Command::TakeResult {
                    request: input_request
                }
            ),
            Response::Pending
        ));
        let mut provider_receipts = 0;
        let mut events = Vec::new();
        let mut lost_once = false;
        let mut result = None;
        for _ in 0..1500 {
            inbox.prune_unstarted(&mut |cmd| Ok(broker.dispatch("effect", cmd)));
            inbox.advance(&mut host.pipeline.records, true, &mut |cmd| {
                let is_take = matches!(cmd, Command::TakeResult { .. });
                let reply = broker.dispatch("effect", cmd);
                if is_take && matches!(reply, Response::Result { .. }) && !lost_once {
                    lost_once = true;
                    Err("reply lost".into())
                } else {
                    Ok(reply)
                }
            });
            for provider in ["pairing", "control"] {
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
                let action = payload["action"].as_str().unwrap();
                let result: Result<Value, &str> = if action == "retire" {
                    provider_receipts += 1;
                    control.handle("effect", payload.clone())
                } else {
                    apply_pairing(&mut gate, &payload)
                };
                let response = result.unwrap_or_else(|code| json!({"ok":false,"code":code}));
                events.push(json!({"action":action,"response":response}));
                assert!(matches!(
                    broker.dispatch(
                        provider,
                        Command::Complete {
                            request: id,
                            payload: response
                        }
                    ),
                    Response::Ok
                ));
            }
            if let Response::Result { payload } = broker.dispatch(
                "input",
                Command::TakeResult {
                    request: input_request,
                },
            ) {
                result = Some(payload);
            }
            if result.is_some() && !gate.drain_required() {
                break;
            }
        }
        let result = result.unwrap_or_else(|| panic!("events: {events:?}"));
        assert_eq!(result["source_count"], 2);
        assert_eq!(result["pairing_retired"], true);
        assert_eq!(result["retirement"]["stream"], stream);
        assert!(lost_once);
        assert_eq!(provider_receipts, 1);
        control.tick();
        let mut buffer = [0u8; 64];
        assert_eq!(
            receiver.recv_from(&mut buffer).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(!gate.drain_required());
        assert_eq!(gate.in_flight(), 0);
        assert_eq!(gate.pending_source_retirements(), 0);
        gate.recover("replacement-owner").unwrap();
        assert!(gate.require_epoch(&epoch).is_err());
        assert!(
            gate.bind_scoped(source(1), Some(selected_scope(1)))
                .is_err()
        );
    }

    #[test]
    fn permission_fence_arriving_during_source_retirement_is_drained() {
        use super::actual_pairing_ledger::EffectGate;
        use amaterasu_contracts::{Grants, RuntimeConfig};
        use amaterasu_runtime::Broker;
        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver.set_nonblocking(true).unwrap();
        let mut control = amaterasu_control_transport::test_support::ControlFixture::new("effect");
        let opened = control.open("effect").unwrap();
        let stream = opened["stream"].as_u64().unwrap();
        let transport_epoch = opened["retirement_epoch"].as_str().unwrap().to_owned();
        control
            .stage_feedback("effect", stream, receiver.local_addr().unwrap())
            .unwrap();
        let selected_scope = |slot| {
            let mut selected = scope(slot);
            selected.transport_service = "amaterasu.control-transport/1".into();
            selected.transport_stream = stream;
            selected.transport_epoch = transport_epoch.clone();
            selected
        };
        let mut gate = EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let epoch = gate.epoch().to_owned();
        let mut registry = Registry::default();
        for slot in [1, 2] {
            gate.bind_scoped(source(slot), Some(selected_scope(slot)))
                .unwrap();
            registry
                .reserve(
                    epoch.clone(),
                    source(slot),
                    selected_scope(slot),
                    binding(),
                    format!("lease-{slot}"),
                    u64::from(slot),
                )
                .unwrap();
        }
        let ticket = gate
            .admit_stream_lease(&source(1), "lease-1", 1, selected_scope(1))
            .unwrap()
            .to_owned();
        registry.accept_admission(&source(1),&json!({"ok":true,"epoch":epoch,"operation_id":"lease-1","ticket":ticket,"kind":"stream_lease","source":source(1),"scope":selected_scope(1)})).unwrap();
        use crate::pipeline::{
            record::Resource,
            tests::harness::{Harness, prepare, start},
        };
        let mut host = Harness::new();
        let session = host.response(1, prepare())["session"].as_u64().unwrap();
        assert_eq!(host.response(2, start(session))["state"], "active");
        {
            let record = host.pipeline.records.get_mut(&session).unwrap();
            record.lifecycle_input_token = Some("binding".into());
            record.start.as_mut().unwrap().lifecycle_target = Some(binding().target);
            record.resources.insert(Resource::Control, stream);
            record.retain_control_retirement_epoch(&json!({"retirement_epoch":transport_epoch}));
        }
        let mut inbox = super::super::Inbox {
            registry,
            ..Default::default()
        };
        let mut config = RuntimeConfig::default();
        config.principals.insert(
            "input".into(),
            Grants {
                call: ["effect/1".into()].into(),
                ..Default::default()
            },
        );
        config.principals.insert(
            "effect".into(),
            Grants {
                provide: ["effect/1".into()].into(),
                call: [
                    effect_authority::SERVICE.into(),
                    "amaterasu.control-transport/1".into(),
                ]
                .into(),
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
            "control".into(),
            Grants {
                provide: ["amaterasu.control-transport/1".into()].into(),
                ..Default::default()
            },
        );
        let mut broker = Broker::new(config).unwrap();
        for (principal, service) in [
            ("pairing", effect_authority::SERVICE),
            ("control", "amaterasu.control-transport/1"),
            ("effect", "effect/1"),
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
        let request_payload = json!({"action":"retire_input_feedback","version":1,"operation_id":"input-retire","token":"binding","target":binding().target,"input_boot_epoch":"b".repeat(32)});
        let Response::Accepted { id: input_request } = broker.dispatch(
            "input",
            Command::Request {
                service: "effect/1".into(),
                payload: request_payload,
                timeout_ms: 5000,
                operation_id: "input-retire".into(),
                parent: None,
            },
        ) else {
            panic!("input admission")
        };
        let Response::Request {
            caller,
            initiator,
            payload,
            ..
        } = broker.dispatch("effect", Command::Poll {})
        else {
            panic!("input delivery")
        };
        inbox.receive(
            input_request,
            &caller,
            &initiator,
            Some("input"),
            payload,
            &mut |cmd| Ok(broker.dispatch("effect", cmd)),
        );
        assert!(matches!(
            broker.dispatch(
                "input",
                Command::TakeResult {
                    request: input_request
                }
            ),
            Response::Pending
        ));
        let mut provider_receipts = 0;
        let mut events = Vec::new();
        let mut lost_once = false;
        let mut result = None;
        let mut mutation_requested = false;
        for _ in 0..1500 {
            inbox.prune_unstarted(&mut |cmd| Ok(broker.dispatch("effect", cmd)));
            inbox.advance(&mut host.pipeline.records, true, &mut |cmd| {
                let is_take = matches!(cmd, Command::TakeResult { .. });
                let reply = broker.dispatch("effect", cmd);
                if is_take && matches!(reply, Response::Result { .. }) && !lost_once {
                    lost_once = true;
                    Err("reply lost".into())
                } else {
                    Ok(reply)
                }
            });
            for provider in ["pairing", "control"] {
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
                let action = payload["action"].as_str().unwrap();
                let result: Result<Value, &str> = if action == "retire" {
                    provider_receipts += 1;
                    control.handle("effect", payload.clone())
                } else {
                    apply_pairing(&mut gate, &payload)
                };
                let response = result.unwrap_or_else(|code| json!({"ok":false,"code":code}));
                if action == "fence_source" && !mutation_requested {
                    gate.fence();
                    mutation_requested = true;
                    assert!(gate.drain_required());
                }
                events.push(json!({"action":action,"response":response}));
                assert!(matches!(
                    broker.dispatch(
                        provider,
                        Command::Complete {
                            request: id,
                            payload: response
                        }
                    ),
                    Response::Ok
                ));
            }
            if let Response::Result { payload } = broker.dispatch(
                "input",
                Command::TakeResult {
                    request: input_request,
                },
            ) {
                result = Some(payload);
            }
            if result.is_some() && !gate.drain_required() {
                break;
            }
        }
        let result = result.unwrap_or_else(|| panic!("events: {events:?}"));
        assert_eq!(result["source_count"], 2);
        assert_eq!(result["pairing_retired"], true);
        assert_eq!(result["retirement"]["stream"], stream);
        assert!(lost_once);
        assert_eq!(provider_receipts, 1);
        control.tick();
        let mut buffer = [0u8; 64];
        assert_eq!(
            receiver.recv_from(&mut buffer).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(mutation_requested);
        assert!(!gate.drain_required());
        assert_eq!(gate.in_flight(), 0);
        assert_eq!(gate.pending_source_retirements(), 0);
        assert!(
            gate.bind_scoped(source(1), Some(selected_scope(1)))
                .is_err()
        );
    }

    #[test]
    fn mismatched_transport_incarnation_never_settles_or_acknowledges() {
        let mut registry = Registry::default();
        for slot in [1, 2] {
            registry
                .reserve(
                    "e".repeat(32),
                    source(slot),
                    scope(slot),
                    binding(),
                    format!("lease-{slot}"),
                    1,
                )
                .unwrap();
        }
        let admission = json!({"ok":true,"epoch":"e".repeat(32),"operation_id":"lease-1","ticket":"1".repeat(32),"kind":"stream_lease","source":source(1),"scope":scope(1)});
        registry.accept_admission(&source(1), &admission).unwrap();
        let (transport, records) = registry.take_transport(&binding()).unwrap();
        let mut worker = Worker::new(binding(), transport, records);
        let mut fixture = BrokerFixture {
            wrong_epoch: true,
            ..Default::default()
        };
        for _ in 0..100 {
            assert!(worker.advance(&mut |cmd| fixture.send(cmd)).is_none());
        }
        assert_eq!(fixture.completed, 0);
        assert_eq!(fixture.acked, 0);
        assert_eq!(fixture.sources, 0);
    }

    #[test]
    fn drains_all_sources_before_ack_and_reconciles_unknown_admission() {
        let mut registry = Registry::default();
        for slot in [1, 2] {
            registry
                .reserve(
                    "e".repeat(32),
                    source(slot),
                    scope(slot),
                    binding(),
                    format!("lease-{slot}"),
                    1,
                )
                .unwrap();
        }
        let admission = json!({"ok":true,"epoch":"e".repeat(32),"operation_id":"lease-1","ticket":"1".repeat(32),"kind":"stream_lease","source":source(1),"scope":scope(1)});
        registry.accept_admission(&source(1), &admission).unwrap();
        let (transport, records) = registry.take_transport(&binding()).unwrap();
        let mut worker = Worker::new(binding(), transport, records);
        let mut fixture = BrokerFixture::default();
        let mut result = None;
        for _ in 0..1000 {
            if let Some(receipt) = worker.advance(&mut |cmd| fixture.send(cmd)) {
                result = Some(receipt);
                break;
            }
        }
        let receipt = result.expect("whole transport retired");
        assert_eq!(receipt.stream, 7);
        assert_eq!(fixture.fences, 2);
        assert_eq!(fixture.sources, 2);
        assert_eq!(worker.source_count(), 2);
    }
}

// This test compiles the actual pairing ledger in place. Its control provider
// reply is synthetic; it is not an end-to-end ENet retirement proof.
#[cfg(test)]
#[allow(dead_code, unused_imports, clippy::duplicate_mod)]
#[path = "../../../../pairing/src/effect_gate.rs"]
mod actual_pairing_ledger;
