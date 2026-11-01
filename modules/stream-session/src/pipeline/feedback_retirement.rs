//! Bounded input-retirement ingress. Accepted requests are never acknowledgments:
//! they remain pending until the paired source registry and transport are drained.
mod admission;
mod registry;
mod restart;
mod worker;

use super::{Send, record::Record, rpc};
use amaterasu_extension_api::LifecycleTarget;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant},
};

const MAX_RETIREMENTS: usize = 16;
const MAX_WAITERS: usize = 4;
const MAX_COMPLETED: usize = 4096;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Request {
    action: String,
    version: u8,
    operation_id: String,
    token: String,
    target: LifecycleTarget,
    input_boot_epoch: String,
}
impl Request {
    fn valid(&self) -> bool {
        fn token(value: &str, limit: usize) -> bool {
            !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
        }
        let stream = match &self.target {
            LifecycleTarget::Stream {
                host_epoch,
                session_id,
                stream_id,
                generation,
            } => {
                token(host_epoch, 128)
                    && token(session_id, 128)
                    && token(stream_id, 128)
                    && *generation != 0
            }
            _ => false,
        };
        self.action == "retire_input_feedback"
            && self.version == 1
            && token(&self.operation_id, 128)
            && token(&self.token, 256)
            && stream
            && self.input_boot_epoch.len() == 32
            && self
                .input_boot_epoch
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
}
struct Entry {
    request: Request,
    waiters: Vec<u64>,
    worker: Option<worker::Worker>,
    internal: bool,
}
#[derive(Default)]
pub(super) struct Inbox {
    registry: registry::Registry,
    admission: Option<admission::Admission>,
    next_admission_sequence: u64,
    entries: VecDeque<Entry>,
    completed: BTreeMap<String, (Request, Value)>,
    retired_bindings: Vec<(
        registry::Binding,
        amaterasu_module_sdk::effect_authority::StreamRetirement,
        usize,
    )>,
    authority_fenced: bool,
    watch_counter: u64,
    watch_call: Option<worker::Call>,
    watch_started: Option<Instant>,
    watch_context: Option<(String, String, registry::Binding, registry::Transport)>,
    last_context: Option<(String, String, registry::Binding, registry::Transport)>,
    source_scan: VecDeque<(
        String,
        amaterasu_module_sdk::effect_authority::Source,
        amaterasu_module_sdk::effect_authority::StreamScope,
        registry::Binding,
        registry::Transport,
    )>,
    scan_call: Option<worker::Call>,
    drain: Option<(String, u64)>,
    drain_call: Option<worker::Call>,
    restart_required: bool,
    restart: Option<restart::Restart>,
}
impl Inbox {
    pub fn new(restart_required: bool, selected_transport: String) -> Self {
        Self {
            restart_required,
            restart: restart_required
                .then(|| restart::Restart::new(selected_transport))
                .flatten(),
            ..Default::default()
        }
    }
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    #[cfg(test)]
    pub(super) fn install_test_source(
        &mut self,
        target: LifecycleTarget,
        source: amaterasu_module_sdk::effect_authority::Source,
        scope: amaterasu_module_sdk::effect_authority::StreamScope,
    ) {
        let epoch = "e".repeat(32);
        let binding = registry::Binding {
            token: "binding".into(),
            target,
            input_boot_epoch: "b".repeat(32),
        };
        self.registry
            .reserve(
                epoch.clone(),
                source.clone(),
                scope.clone(),
                binding,
                "test-lease".into(),
                1,
            )
            .unwrap();
        self.registry.accept_admission(&source,&json!({"ok":true,"epoch":epoch,"operation_id":"test-lease","ticket":"f".repeat(32),"kind":"stream_lease","source":source,"scope":scope})).unwrap();
    }
    /// This verifies broker identity and request shape only. No payload value is
    /// treated as evidence of a source, admitted lease or transport retirement.
    pub fn receive(
        &mut self,
        request_id: u64,
        caller: &str,
        initiator: &str,
        selected: Option<&str>,
        payload: Value,
        send: &mut Send<'_>,
    ) {
        if selected != Some(caller) || initiator != caller {
            rpc::complete(
                send,
                request_id,
                json!({"ok":false,"code":"permission_denied"}),
            );
            return;
        }
        let Ok(request) = serde_json::from_value::<Request>(payload) else {
            rpc::complete(
                send,
                request_id,
                json!({"ok":false,"code":"invalid_request"}),
            );
            return;
        };
        if !request.valid() {
            rpc::complete(
                send,
                request_id,
                json!({"ok":false,"code":"invalid_request"}),
            );
            return;
        }
        if let Some((old, reply)) = self.completed.get(&request.operation_id) {
            if old != &request {
                rpc::complete(
                    send,
                    request_id,
                    json!({"ok":false,"code":"operation_conflict"}),
                );
            } else {
                rpc::complete(send, request_id, reply.clone());
            }
            return;
        }
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.request.operation_id == request.operation_id)
        {
            if entry.request != request {
                rpc::complete(
                    send,
                    request_id,
                    json!({"ok":false,"code":"operation_conflict"}),
                );
            } else if entry.waiters.len() >= MAX_WAITERS {
                rpc::complete(
                    send,
                    request_id,
                    json!({"ok":false,"code":"resource_exhausted"}),
                );
            } else if !entry.waiters.contains(&request_id) {
                entry.waiters.push(request_id);
            }
            return;
        }
        if self.entries.len() >= MAX_RETIREMENTS {
            rpc::complete(
                send,
                request_id,
                json!({"ok":false,"code":"resource_exhausted"}),
            );
            return;
        }
        self.entries.push_back(Entry {
            request,
            waiters: vec![request_id],
            worker: None,
            internal: false,
        });
    }
    pub fn prune_unstarted(&mut self, send: &mut Send<'_>) {
        for entry in &mut self.entries {
            entry.waiters.retain(|id| rpc::active(send, *id));
        }
        // This inbox has not yet issued a source fence or touched a provider.
        // Once it owns work, its worker must retain the entry after cancellation.
        self.entries
            .retain(|entry| entry.internal || entry.worker.is_some() || !entry.waiters.is_empty());
    }
    /// Stop the local sender before an arrival, removal or haptics-disable
    /// packet can retire a device incarnation. The whole control stream is
    /// then retired by the same source/transport barrier.
    pub fn preflight_input(
        &mut self,
        record: &mut Record,
        input: &amaterasu_gamestream::control::Input<'_>,
    ) -> bool {
        let Some(target) = record
            .start
            .as_ref()
            .and_then(|start| start.lifecycle_target.as_ref())
        else {
            return false;
        };
        let Some((binding, transport)) = self.registry.affected_by_input(target, input) else {
            return false;
        };
        let _ = self.registry.fence_local_transport(&transport);
        self.enqueue_internal(binding, &transport);
        super::input::stop_for_input_failure(record, "controller_source_retirement");
        record.client_haptics_requested = false;
        record.rumble_pending.clear();
        true
    }
    pub fn route_feedback(
        &mut self,
        record: &mut Record,
        slot: u16,
        generation: u64,
        sequence: u64,
        feedback: amaterasu_gamestream::control::HostFeedback,
    ) -> Result<bool, &'static str> {
        if self.authority_fenced || !self.source_scan.is_empty() || self.scan_call.is_some() {
            return Ok(false);
        }
        let Some(token) = record.lifecycle_input_token.as_ref() else {
            return Ok(false);
        };
        let Some(target) = record
            .start
            .as_ref()
            .and_then(|start| start.lifecycle_target.clone())
        else {
            return Ok(false);
        };
        let Some(boot) = record
            .feedback_source_observations
            .get(&slot)
            .and_then(|observation| observation["input_boot_epoch"].as_str())
        else {
            return Ok(false);
        };
        let binding = registry::Binding {
            token: token.clone(),
            target,
            input_boot_epoch: boot.into(),
        };
        let Some(plane) = record.plane.as_mut() else {
            return Err("feedback_plane_unavailable");
        };
        self.registry.route_feedback(
            &binding,
            slot,
            generation,
            sequence,
            feedback,
            plane.as_mut(),
        )
    }
    pub fn acknowledge_feedback(
        &mut self,
        record: &Record,
        through: u64,
    ) -> Result<(), &'static str> {
        let Some(token) = record.lifecycle_input_token.as_ref() else {
            return Ok(());
        };
        let Some(target) = record
            .start
            .as_ref()
            .and_then(|start| start.lifecycle_target.clone())
        else {
            return Ok(());
        };
        for observation in record.feedback_source_observations.values() {
            if let Some(boot) = observation["input_boot_epoch"].as_str() {
                let binding = registry::Binding {
                    token: token.clone(),
                    target: target.clone(),
                    input_boot_epoch: boot.into(),
                };
                self.registry.acknowledge_feedback(&binding, through)?;
            }
        }
        Ok(())
    }
    fn enqueue_internal(&mut self, binding: registry::Binding, transport: &registry::Transport) {
        if self.entries.iter().any(|entry| {
            entry.request.token == binding.token
                && entry.request.target == binding.target
                && entry.request.input_boot_epoch == binding.input_boot_epoch
        }) || self.entries.len() >= MAX_RETIREMENTS
        {
            return;
        }
        let request = Request {
            action: "retire_input_feedback".into(),
            version: 1,
            operation_id: format!("owner-retire-{}-{}", transport.epoch, transport.stream),
            token: binding.token,
            target: binding.target,
            input_boot_epoch: binding.input_boot_epoch,
        };
        self.entries.push_back(Entry {
            request,
            waiters: Vec::new(),
            worker: None,
            internal: true,
        });
    }
    fn progress_authority(&mut self, allow: bool, send: &mut Send<'_>) {
        if !allow {
            return;
        }
        if self
            .watch_started
            .is_some_and(|started| started.elapsed() >= Duration::from_secs(5))
        {
            self.authority_fenced = true;
            if let Some((_, _, binding, transport)) = self.watch_context.clone() {
                self.enqueue_internal(binding, &transport);
            }
            if self
                .watch_call
                .as_mut()
                .is_some_and(|call| call.settle(send))
            {
                self.watch_call = None;
                self.watch_started = None;
            }
            return;
        }
        if let Some((epoch, generation)) = self.drain.clone() {
            if let Some((_, _, binding, transport)) = self.registry.first_source() {
                self.enqueue_internal(binding, &transport);
                return;
            }
            if self.entries.iter().any(|entry| entry.worker.is_some()) {
                return;
            }
            if self.drain_call.is_none() {
                self.drain_call = Some(worker::Call::new(
                    amaterasu_module_sdk::effect_authority::SERVICE,
                    json!({"action":"drain","epoch":epoch,"fence_generation":generation}),
                    format!("feedback-global-drain-{epoch}-{generation}"),
                ));
            }
            if let Some(response) = self.drain_call.as_mut().and_then(|call| call.poll(send)) {
                self.drain_call = None;
                if response["ok"] == true
                    && response["epoch"] == epoch
                    && response["fence_generation"] == generation
                {
                    self.drain = None;
                }
            }
            return;
        }
        if self.authority_fenced {
            if let Some((_, _, binding, transport)) = self.registry.first_source() {
                self.enqueue_internal(binding, &transport);
            }
            return;
        }
        if let Some((epoch, source, scope, binding, transport)) = self.source_scan.front().cloned()
        {
            if self.scan_call.is_none() {
                let Some(next) = self.watch_counter.checked_add(1) else {
                    self.authority_fenced = true;
                    return;
                };
                self.watch_counter = next;
                let payload = json!({"action":"lookup_current_source_retirement","epoch":epoch,"source":source,"scope":scope});
                self.scan_call = Some(worker::Call::new(
                    amaterasu_module_sdk::effect_authority::SERVICE,
                    payload,
                    format!("feedback-source-watch-{next}"),
                ));
            }
            if let Some(response) = self.scan_call.as_mut().and_then(|call| call.poll(send)) {
                self.scan_call = None;
                if response["ok"] == false && response["code"] == "unknown_source_retirement" {
                    self.source_scan.pop_front();
                } else if let Ok(receipt) = serde_json::from_value::<
                    amaterasu_module_sdk::effect_source_retirement::Receipt,
                >(response)
                    && receipt.matches_source(&epoch, &source, &scope)
                {
                    self.source_scan.pop_front();
                    self.enqueue_internal(binding, &transport);
                }
            }
            return;
        }
        if self.watch_call.is_none() {
            let Some(context) = self
                .registry
                .first_source()
                .or_else(|| {
                    self.entries.iter().find_map(|entry| {
                        entry
                            .worker
                            .as_ref()
                            .and_then(|worker| worker.watch_context())
                    })
                })
                .or_else(|| self.last_context.clone())
            else {
                return;
            };
            let Some(next) = self.watch_counter.checked_add(1) else {
                self.authority_fenced = true;
                return;
            };
            self.watch_counter = next;
            self.last_context = Some(context.clone());
            self.watch_context = Some(context);
            self.watch_started = Some(Instant::now());
            self.watch_call = Some(worker::Call::new(
                amaterasu_module_sdk::effect_authority::SERVICE,
                json!({"action":"status"}),
                format!("feedback-watch-{next}"),
            ));
        }
        if let Some(response) = self.watch_call.as_mut().and_then(|call| call.poll(send)) {
            self.watch_call = None;
            self.watch_started = None;
            let Some((epoch, owner, binding, transport)) = self.watch_context.take() else {
                self.authority_fenced = true;
                return;
            };
            let valid = response["ok"] == true
                && response["epoch"] == epoch
                && response["owner_epoch"] == owner;
            let fenced = !valid
                || response["fenced"] != false
                || response["drain_required"] != false
                || response["startup_recovery_required"] != false;
            let source_fenced = response["pending_source_retirements"]
                .as_u64()
                .is_none_or(|n| n > 0);
            if fenced {
                self.authority_fenced = true;
                if valid
                    && let Some(generation) =
                        response["fence_generation"].as_u64().filter(|n| *n > 0)
                {
                    self.drain = Some((epoch, generation));
                }
            }
            if fenced {
                self.enqueue_internal(binding, &transport);
            } else if source_fenced {
                self.source_scan = self.registry.source_snapshots().into();
            } else if self.registry.first_source().is_none()
                && !self.entries.iter().any(|entry| entry.worker.is_some())
            {
                self.last_context = None;
            }
        }
    }
    /// Progresses independently of the session workflow. A source is eligible
    /// only when an owner-held registry reservation and the actual record agree.
    pub fn advance(
        &mut self,
        records: &mut BTreeMap<u64, Record>,
        selected_protocol: Option<&str>,
        send: &mut Send<'_>,
    ) {
        if self.restart_required {
            if self
                .restart
                .as_mut()
                .is_some_and(|restart| restart.advance(send))
            {
                self.restart_required = false;
                self.restart = None;
            } else {
                return;
            }
        }
        let allow_admission = selected_protocol.is_some();
        if allow_admission && let Some((binding, transport)) = self.registry.first_stale(records) {
            self.enqueue_internal(binding, &transport);
        }
        self.progress_authority(allow_admission, send);
        if self.entries.is_empty()
            && allow_admission
            && !self.authority_fenced
            && self.source_scan.is_empty()
            && self.scan_call.is_none()
        {
            if self.admission.is_none() {
                let next = self.next_admission_sequence.checked_add(1);
                let retired: Vec<_> = self
                    .retired_bindings
                    .iter()
                    .map(|(binding, _, _)| binding.clone())
                    .collect();
                if let Some(sequence) = next
                    && let Some(admission) = admission::Admission::find(
                        records,
                        &self.registry,
                        sequence,
                        &retired,
                        selected_protocol.expect("admission enabled"),
                    )
                {
                    self.next_admission_sequence = sequence;
                    self.admission = Some(admission);
                }
            }
            if let Some(admission) = self.admission.as_mut()
                && admission.advance(records, &mut self.registry, send)
            {
                self.admission = None;
            }
        }
        let Some(entry) = self.entries.front_mut() else {
            return;
        };
        if entry.worker.is_none() {
            if self.completed.len() >= MAX_COMPLETED || self.retired_bindings.len() >= MAX_COMPLETED
            {
                return;
            }
            let binding = registry::Binding {
                token: entry.request.token.clone(),
                target: entry.request.target.clone(),
                input_boot_epoch: entry.request.input_boot_epoch.clone(),
            };
            if !self.registry.has_binding(&binding)
                && let Some((_, receipt, count)) = self
                    .retired_bindings
                    .iter()
                    .find(|(old, _, _)| old == &binding)
            {
                let reply = json!({"ok":true,"status":"retired","token":entry.request.token,"target":entry.request.target,"input_boot_epoch":entry.request.input_boot_epoch,"operation_id":entry.request.operation_id,"pairing_retired":true,"source_count":count,"retirement":receipt});
                let entry = self.entries.pop_front().expect("retired entry");
                self.completed.insert(
                    entry.request.operation_id.clone(),
                    (entry.request, reply.clone()),
                );
                for waiter in entry.waiters {
                    rpc::complete(send, waiter, reply.clone());
                }
                return;
            }
            let transport = match self.registry.transport_for(&binding) {
                Ok(transport) => transport,
                Err(_) => {
                    let Some(record) = records.values().find(|record| {
                        record.lifecycle_input_token.as_deref() == Some(binding.token.as_str())
                            && record
                                .start
                                .as_ref()
                                .and_then(|start| start.lifecycle_target.as_ref())
                                == Some(&binding.target)
                    }) else {
                        return;
                    };
                    let Some((stream, service, epoch)) = record.control_retirement() else {
                        return;
                    };
                    let transport = registry::Transport {
                        service: service.into(),
                        stream,
                        epoch: epoch.into(),
                    };
                    if self
                        .admission
                        .as_ref()
                        .is_some_and(|admission| admission.belongs_to(&transport))
                    {
                        if !self
                            .admission
                            .as_mut()
                            .expect("checked admission")
                            .settle(send)
                        {
                            return;
                        }
                        self.admission = None;
                    }
                    transport
                }
            };
            if self
                .retired_bindings
                .len()
                .saturating_add(self.registry.bindings_on(&transport))
                > MAX_COMPLETED
            {
                return;
            }
            let record = records.values_mut().find(|record| {
                record
                    .start
                    .as_ref()
                    .and_then(|start| start.lifecycle_target.as_ref())
                    == Some(&binding.target)
                    && record.control_retirement()
                        == Some((
                            transport.stream,
                            transport.service.as_str(),
                            transport.epoch.as_str(),
                        ))
            });
            if record.as_ref().is_some_and(|record| {
                record.plane.is_none() && record.state != super::record::State::Stopping
            }) {
                return;
            }
            let Ok(sources) = self.registry.take_exact_transport(&transport) else {
                return;
            };
            let selected = transport;
            if let Some(record) = record {
                if let Some(plane) = record.plane.as_mut() {
                    plane.stop();
                }
                record.client_haptics_requested = false;
                record.rumble_pending.clear();
            }
            entry.worker = Some(worker::Worker::new(binding, selected, sources));
        }
        let Some(worker) = entry.worker.as_mut() else {
            return;
        };
        let Some(receipt) = worker.advance(send) else {
            return;
        };
        if self
            .admission
            .as_ref()
            .is_some_and(|admission| admission.belongs_to(worker.transport()))
        {
            if !self
                .admission
                .as_mut()
                .expect("checked admission")
                .settle(send)
            {
                return;
            }
            self.admission = None;
        }
        let reply = json!({"ok":true,"status":"retired","token":entry.request.token,
            "target":entry.request.target,"input_boot_epoch":entry.request.input_boot_epoch,
            "operation_id":entry.request.operation_id,"pairing_retired":true,
            "source_count":worker.source_count(),"retirement":receipt});
        for binding in worker.bindings() {
            if !self
                .retired_bindings
                .iter()
                .any(|(old, _, _)| old == &binding)
            {
                self.retired_bindings
                    .push((binding, receipt.clone(), worker.source_count()));
            }
        }
        let entry = self.entries.pop_front().expect("completed entry");
        self.completed.insert(
            entry.request.operation_id.clone(),
            (entry.request, reply.clone()),
        );
        for waiter in entry.waiters {
            rpc::complete(send, waiter, reply.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::{Command, Response};
    fn payload(operation: &str) -> Value {
        json!({"action":"retire_input_feedback","version":1,"operation_id":operation,"token":"binding","target":{"kind":"stream","host_epoch":"host","session_id":"session","stream_id":"stream","generation":1},"input_boot_epoch":"a".repeat(32)})
    }
    #[test]
    fn authority_status_timeout_stops_admission_without_acknowledging_retirement() {
        use amaterasu_module_sdk::effect_authority::{
            FeedbackCapability, FeedbackDirection, Source, StreamScope,
        };
        let target = LifecycleTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 2,
        };
        let source = Source {
            version: 1,
            id: "a".repeat(32),
            generation: 2,
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            stream_generation: 2,
            device: "device".into(),
            device_generation: 1,
            effect_owner_epoch: "owner".into(),
        };
        let scope = StreamScope {
            capabilities: [FeedbackCapability {
                schema: "org.amaterasu.feedback.rumble".into(),
                version: 1,
                direction: FeedbackDirection::ToClient,
            }]
            .into(),
            controller_slot: 3,
            transport_service: "control/1".into(),
            transport_stream: 7,
            transport_epoch: "c".repeat(32),
        };
        let mut inbox = Inbox::default();
        inbox.install_test_source(target, source, scope);
        inbox.watch_context = inbox.registry.first_source();
        inbox.watch_call = Some(worker::Call::new(
            "pairing/1",
            json!({"action":"status"}),
            "watch".into(),
        ));
        inbox.watch_started = Some(Instant::now() - Duration::from_secs(6));
        inbox.progress_authority(true, &mut |_| {
            panic!("unsent watch needs no broker request")
        });
        assert!(inbox.authority_fenced);
        assert_eq!(inbox.entries.len(), 1);
        assert!(inbox.completed.is_empty());
    }

    #[test]
    fn real_broker_input_ack_waits_for_real_control_retirement_without_sources() {
        use crate::pipeline::{
            record::Resource,
            tests::harness::{Harness, prepare, start},
        };
        use amaterasu_contracts::{Grants, RuntimeConfig};
        use amaterasu_runtime::Broker;
        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver.set_nonblocking(true).unwrap();
        let mut control = amaterasu_control_transport::test_support::ControlFixture::new("effect");
        let opened = control.open("effect").unwrap();
        let stream = opened["stream"].as_u64().unwrap();
        let epoch = opened["retirement_epoch"].as_str().unwrap().to_owned();
        control
            .stage_feedback("effect", stream, receiver.local_addr().unwrap())
            .unwrap();
        let mut host = Harness::new();
        let session = host.response(1, prepare())["session"].as_u64().unwrap();
        assert_eq!(host.response(2, start(session))["state"], "active");
        let target = json!({"kind":"stream","host_epoch":"host","session_id":"session","stream_id":"stream","generation":1});
        {
            let record = host.pipeline.records.get_mut(&session).unwrap();
            record.lifecycle_input_token = Some("binding".into());
            record.start.as_mut().unwrap().lifecycle_target =
                Some(serde_json::from_value(target).unwrap());
            record.resources.insert(Resource::Control, stream);
            record.retain_control_retirement_epoch(&json!({"retirement_epoch":epoch}));
        }
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
                call: ["amaterasu.control-transport/1".into()].into(),
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
            ("effect", "effect/1"),
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
        let Response::Accepted { id } = broker.dispatch(
            "input",
            Command::Request {
                service: "effect/1".into(),
                payload: payload("retire"),
                timeout_ms: 5000,
                operation_id: "retire".into(),
                parent: None,
            },
        ) else {
            panic!("request admission");
        };
        let Response::Request {
            caller,
            initiator,
            payload,
            ..
        } = broker.dispatch("effect", Command::Poll {})
        else {
            panic!("request delivery");
        };
        let mut inbox = Inbox::default();
        inbox.receive(
            id,
            &caller,
            &initiator,
            Some("input"),
            payload,
            &mut |cmd| Ok(broker.dispatch("effect", cmd)),
        );
        assert!(matches!(
            broker.dispatch("input", Command::TakeResult { request: id }),
            Response::Pending
        ));
        let mut answer = None;
        for _ in 0..100 {
            inbox.prune_unstarted(&mut |cmd| Ok(broker.dispatch("effect", cmd)));
            inbox.advance(&mut host.pipeline.records, None, &mut |cmd| {
                Ok(broker.dispatch("effect", cmd))
            });
            if let Response::Request {
                id: request,
                caller,
                payload,
                ..
            } = broker.dispatch("control", Command::Poll {})
            {
                assert_eq!(caller, "effect");
                let response = control.handle(&caller, payload).unwrap();
                assert!(matches!(
                    broker.dispatch(
                        "control",
                        Command::Complete {
                            request,
                            payload: response
                        }
                    ),
                    Response::Ok
                ));
            }
            if let Response::Result { payload } =
                broker.dispatch("input", Command::TakeResult { request: id })
            {
                answer = Some(payload);
                break;
            }
        }
        let answer = answer.expect("retirement acknowledged");
        assert_eq!(answer["pairing_retired"], true);
        assert_eq!(answer["source_count"], 0);
        assert_eq!(answer["retirement"]["stream"], stream);
        assert_eq!(answer["retirement"]["provider_epoch"], epoch);
        control.tick();
        let mut buffer = [0u8; 64];
        assert_eq!(
            receiver.recv_from(&mut buffer).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn real_broker_keeps_ingress_pending_until_cancelled() {
        use amaterasu_contracts::{Grants, RuntimeConfig};
        use amaterasu_runtime::Broker;
        let mut config = RuntimeConfig::default();
        config.principals.insert(
            "input".into(),
            Grants {
                call: ["owner/1".into()].into(),
                ..Default::default()
            },
        );
        config.principals.insert(
            "owner".into(),
            Grants {
                provide: ["owner/1".into()].into(),
                ..Default::default()
            },
        );
        let mut broker = Broker::new(config).unwrap();
        assert!(matches!(
            broker.dispatch(
                "owner",
                Command::Register {
                    service: "owner/1".into()
                }
            ),
            Response::Ok
        ));
        let Response::Accepted { id } = broker.dispatch(
            "input",
            Command::Request {
                service: "owner/1".into(),
                payload: payload("op"),
                timeout_ms: 5000,
                operation_id: "op".into(),
                parent: None,
            },
        ) else {
            panic!("admission");
        };
        let Response::Request {
            caller,
            initiator,
            payload,
            ..
        } = broker.dispatch("owner", Command::Poll {})
        else {
            panic!("delivery");
        };
        let mut inbox = Inbox::default();
        inbox.receive(
            id,
            &caller,
            &initiator,
            Some("input"),
            payload,
            &mut |cmd| Ok(broker.dispatch("owner", cmd)),
        );
        assert_eq!(inbox.len(), 1);
        assert!(matches!(
            broker.dispatch("input", Command::TakeResult { request: id }),
            Response::Pending
        ));
        assert!(matches!(
            broker.dispatch("input", Command::Cancel { request: id }),
            Response::Ok
        ));
        inbox.prune_unstarted(&mut |cmd| Ok(broker.dispatch("owner", cmd)));
        assert_eq!(inbox.len(), 0);
    }

    #[test]
    fn only_direct_selected_input_can_queue_and_retry_is_exact() {
        let mut inbox = Inbox::default();
        let mut replies = Vec::new();
        let mut send = |command| {
            if let Command::Complete { request, payload } = command {
                replies.push((request, payload));
            }
            Ok(Response::Ok)
        };
        inbox.receive(
            1,
            "input",
            "attacker",
            Some("input"),
            payload("op"),
            &mut send,
        );
        inbox.receive(2, "other", "other", Some("input"), payload("op"), &mut send);
        inbox.receive(3, "input", "input", Some("input"), payload("op"), &mut send);
        inbox.receive(4, "input", "input", Some("input"), payload("op"), &mut send);
        let mut conflict = payload("op");
        conflict["token"] = "other".into();
        inbox.receive(5, "input", "input", Some("input"), conflict, &mut send);
        assert_eq!(inbox.len(), 1);
        assert_eq!(replies.len(), 3);
        assert_eq!(replies[0].1["code"], "permission_denied");
        assert_eq!(replies[2].1["code"], "operation_conflict");
    }
    #[test]
    fn bounded_queue_fails_closed_and_unstarted_cancellation_releases_capacity() {
        let mut inbox = Inbox::default();
        let mut replies = Vec::new();
        let mut send = |command| {
            match command {
                Command::Complete { request, payload } => replies.push((request, payload)),
                Command::CheckRequest { .. } => {
                    return Ok(Response::Error {
                        code: "request_ended".into(),
                        message: String::new(),
                    });
                }
                _ => {}
            }
            Ok(Response::Ok)
        };
        for n in 0..=MAX_RETIREMENTS {
            inbox.receive(
                n as u64,
                "input",
                "input",
                Some("input"),
                payload(&format!("op-{n}")),
                &mut send,
            );
        }
        assert_eq!(inbox.len(), MAX_RETIREMENTS);
        inbox.prune_unstarted(&mut send);
        assert_eq!(inbox.len(), 0);
        assert_eq!(replies[0].1["code"], "resource_exhausted");
    }
}
