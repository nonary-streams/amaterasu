use super::super::*;
use std::{
    cell::RefCell,
    collections::{BTreeSet, VecDeque},
    rc::Rc,
};

#[derive(Clone)]
pub struct Call {
    pub service: String,
    pub payload: Value,
    pub parent: Option<u64>,
}
struct Request {
    call: Call,
    result: Option<Value>,
    cancelled: bool,
}
#[derive(Clone)]
struct Allocation {
    id: u64,
    key: String,
    state: String,
    leases: serde_json::Map<String, Value>,
    request: Value,
}
#[derive(Clone, Copy)]
pub enum Fault {
    BeforeMutation,
    AfterMutation,
    Admission,
}

#[derive(Default)]
pub struct Broker {
    incoming: VecDeque<Response>,
    requests: BTreeMap<u64, Request>,
    operations: BTreeMap<String, u64>,
    allocations: BTreeMap<(String, String), Allocation>,
    cancelled: BTreeSet<u64>,
    next: u64,
    pub replies: BTreeMap<u64, Value>,
    pub calls: Vec<Call>,
    pub commands: Vec<Command>,
    pub fault: Option<(String, Fault)>,
    pub fail_cancel: bool,
    pub fail_open: Option<String>,
    pub fail_close: Option<String>,
    pub fail_audio_recover: bool,
    pub malformed_audio_recover: bool,
    pub authenticated: bool,
    pub video_close_polls: u8,
    pub video_remaining: u8,
    pub format_rate: u32,
    pub fail_completion: bool,
    pub discovered_video_port: Option<u16>,
    pub discovered_audio_port: Option<u16>,
    pub media_failure: bool,
    pub desktop_geometry: Option<Value>,
    pub touch_port: Option<Value>,
    pub input_operation: Option<String>,
    pub input_lease: Option<String>,
    pub input_ttl_ms: u64,
    pub input_terminal: bool,
    pub input_events: Vec<Value>,
    pub input_feedback_events: VecDeque<Value>,
    pub input_feedback_acks: Vec<u64>,
    pub feedback_ack_failures: usize,
    pub fail_input_batch: bool,
    pub fail_controller_input: bool,
    pub input_cleanup_required: bool,
    pub input_expires_in_ms: Option<u64>,
    pub input_absolute_pointer: Option<Value>,
    pub input_controller_supported: Option<Value>,
    pub input_pen_touch_supported: Option<Value>,
    pub input_native_pen_touch: Option<Value>,
    pub history_available: bool,
    pub history_storage_degraded: bool,
    pub history_recording_disabled: bool,
    pub history_failed_actions: BTreeSet<String>,
    pub history_pending_polls: usize,
    pub history_release_failures: usize,
    pub history_requests: BTreeSet<u64>,
    pub history_released: usize,
    pub history_release_attempts: usize,
    pub history_begins: Vec<Value>,
    pub history_ends: Vec<Value>,
    pub history_samples: Vec<Value>,
    pub history_events: Vec<Value>,
    pub history_activities: BTreeSet<String>,
    pub history_activity_ends: Vec<Value>,
    pub host_metrics_available: bool,
    pub host_metrics_activities: BTreeSet<String>,
    pub host_metrics_activity_begins: Vec<Value>,
    pub host_metrics_activity_ends: Vec<Value>,
    pub updater_active_sessions: BTreeSet<String>,
    pub updater_session_starts: Vec<Value>,
    pub updater_session_stops: Vec<Value>,
    pub tray_activity_updates: Vec<Value>,
    pub tray_vigem_notifications: usize,
    pub limiter_unavailable: bool,
    pub limiter_admission_unavailable: bool,
    pub limiter_restore_failures: usize,
    pub limiter_lease: Option<String>,
    pub limiter_session: Option<String>,
    pub limiter_executable_path: Option<String>,
    pub limiter_status_executable_override: Option<String>,
    pub limiter_malformed_status: bool,
    pub display_lease: Option<String>,
    pub display_operation: Option<String>,
    pub display_recovery_required: bool,
    pub display_apply_failure: bool,
    pub display_wrong_apply_operation: bool,
    pub display_apply_recovery_reply: bool,
    pub display_wrong_status_operation: bool,
    pub display_restore_failures: usize,
    pub display_unavailable: bool,
    pub display_race_on_lost_apply: bool,
    pub display_restart_on_lost_apply: bool,
    pub display_inactive_target: Option<String>,
    pub display_active_ids: Vec<String>,
}

impl Broker {
    pub fn new() -> Self {
        Self {
            authenticated: true,
            format_rate: 48000,
            host_metrics_available: true,
            ..Self::default()
        }
    }

    pub fn enqueue(&mut self, request: u64, owner: &str, payload: Value) {
        self.incoming.push_back(Response::Request {
            id: request,
            caller: owner.into(),
            initiator: owner.into(),
            service: SERVICE.into(),
            payload,
            remaining_ms: 30000,
        });
    }

    pub fn cancel_parent(&mut self, request: u64) {
        self.cancelled.insert(request);
    }

    pub fn live_components(&self) -> usize {
        self.allocations
            .keys()
            .filter(|(service, _)| service != "amaterasu.session/1")
            .count()
    }

    pub fn leases(&self) -> usize {
        self.allocations
            .values()
            .map(|value| value.leases.len())
            .sum()
    }

    pub fn history_calls(&self) -> usize {
        self.calls
            .iter()
            .filter(|call| call.service == "amaterasu.session-history/1")
            .count()
    }

    pub fn send(&mut self, command: Command) -> ModuleResult<Response> {
        self.commands.push(command.clone());
        match command {
            Command::Poll {} => Ok(self.incoming.pop_front().unwrap_or(Response::Pending)),
            Command::CheckRequest { request } => Ok(if self.cancelled.contains(&request) {
                Response::Error {
                    code: "request_ended".into(),
                    message: String::new(),
                }
            } else {
                Response::Ok
            }),
            Command::Complete { request, payload } => {
                if self.fail_completion || self.cancelled.contains(&request) {
                    return Ok(Response::Error {
                        code: "request_ended".into(),
                        message: String::new(),
                    });
                }
                self.replies.insert(request, payload);
                Ok(Response::Ok)
            }
            Command::Request {
                service,
                payload,
                parent,
                operation_id,
                ..
            } => {
                if service == "amaterasu.session-history/1" && !self.history_available {
                    return Ok(Response::Error {
                        code: "service_unavailable".into(),
                        message: String::new(),
                    });
                }
                if service == "amaterasu.host-metrics/1" && !self.host_metrics_available {
                    return Ok(Response::Error {
                        code: "service_unavailable".into(),
                        message: String::new(),
                    });
                }
                if service == "amaterasu.frame-limiter/1" && self.limiter_admission_unavailable {
                    return Ok(Response::Error {
                        code: "service_unavailable".into(),
                        message: String::new(),
                    });
                }
                if let Some(id) = self.operations.get(&operation_id) {
                    return Ok(Response::Accepted { id: *id });
                }
                self.next += 1;
                let id = self.next;
                let call = Call {
                    service: service.clone(),
                    payload,
                    parent,
                };
                self.calls.push(call.clone());
                self.requests.insert(
                    id,
                    Request {
                        call,
                        result: None,
                        cancelled: false,
                    },
                );
                if service == "amaterasu.session-history/1" {
                    self.history_requests.insert(id);
                }
                self.operations.insert(operation_id, id);
                if self.fault.as_ref().is_some_and(|(target, mode)| {
                    target == &service && matches!(mode, Fault::Admission)
                }) {
                    self.fault = None;
                    return Err("lost admission".into());
                }
                Ok(Response::Accepted { id })
            }
            Command::TakeResult { request } => {
                let mut pending = self.requests.remove(&request).expect("retained request");
                if pending.cancelled {
                    return Ok(Response::Error {
                        code: "cancelled".into(),
                        message: String::new(),
                    });
                }
                if pending.call.service == "amaterasu.session-history/1"
                    && self.history_pending_polls > 0
                {
                    self.history_pending_polls -= 1;
                    self.requests.insert(request, pending);
                    return Ok(Response::Pending);
                }
                let fault = self
                    .fault
                    .as_ref()
                    .filter(|(service, _)| {
                        service == &pending.call.service
                            || service
                                == &format!(
                                    "{}:{}",
                                    pending.call.service,
                                    pending.call.payload["action"].as_str().unwrap_or("")
                                )
                    })
                    .map(|(_, mode)| *mode);
                if matches!(fault, Some(Fault::BeforeMutation)) {
                    self.fault = None;
                    if self.display_race_on_lost_apply
                        && pending.call.service == "amaterasu.display/1"
                        && pending.call.payload["action"] == "apply"
                    {
                        self.display_lease = Some("display-0000000000000002".into());
                        self.display_operation = Some("another-session-display".into());
                    }
                    self.requests.insert(request, pending);
                    return Err("lost result before provider dispatch".into());
                }
                let result = pending
                    .result
                    .take()
                    .unwrap_or_else(|| self.execute(&pending.call));
                if matches!(fault, Some(Fault::AfterMutation)) {
                    self.fault = None;
                    if self.display_restart_on_lost_apply
                        && pending.call.service == "amaterasu.display/1"
                        && pending.call.payload["action"] == "apply"
                    {
                        self.display_recovery_required = true;
                    }
                    pending.result = Some(result);
                    self.requests.insert(request, pending);
                    return Err("lost result after commit".into());
                }
                self.operations.retain(|_, id| *id != request);
                Ok(Response::Result { payload: result })
            }
            Command::Cancel { request } => {
                if self.fail_cancel {
                    return Err("cancellation transport unavailable".into());
                }
                if let Some(pending) = self.requests.get_mut(&request) {
                    pending.cancelled = true;
                }
                Ok(Response::Ok)
            }
            Command::ReleaseResult { request } => {
                if self.history_requests.contains(&request) {
                    self.history_release_attempts += 1;
                }
                if self.history_requests.contains(&request) && self.history_release_failures > 0 {
                    self.history_release_failures -= 1;
                    return Err("history result release unavailable".into());
                }
                self.requests.remove(&request);
                self.operations.retain(|_, id| *id != request);
                if self.history_requests.remove(&request) {
                    self.history_released += 1;
                }
                Ok(Response::Ok)
            }
            _ => panic!("unexpected broker operation"),
        }
    }

    fn execute(&mut self, call: &Call) -> Value {
        let action = call.payload["action"].as_str().unwrap();
        let service = call.service.as_str();
        if service == "amaterasu.host-metrics/1" {
            return match action {
                "stream_activity_begin" => {
                    self.host_metrics_activity_begins.push(call.payload.clone());
                    let id = call.payload["session_id"].as_str().unwrap().to_owned();
                    self.host_metrics_activities.insert(id);
                    json!({"ok":true,"active":true})
                }
                "stream_activity_end" => {
                    self.host_metrics_activity_ends.push(call.payload.clone());
                    self.host_metrics_activities
                        .remove(call.payload["session_id"].as_str().unwrap());
                    json!({"ok":true,"active":false})
                }
                _ => panic!("unexpected host metrics action"),
            };
        }
        if service == "amaterasu.updater/1" {
            return match action {
                "stream_started" => {
                    self.updater_session_starts.push(call.payload.clone());
                    self.updater_active_sessions
                        .insert(call.payload["session_id"].as_str().unwrap().into());
                    json!({"ok":true})
                }
                "stream_stopped" => {
                    self.updater_session_stops.push(call.payload.clone());
                    self.updater_active_sessions
                        .remove(call.payload["session_id"].as_str().unwrap());
                    json!({"ok":true})
                }
                _ => panic!("unexpected updater action"),
            };
        }
        if service == "amaterasu.system-tray/1" {
            return match action {
                "activity_update" => {
                    self.tray_activity_updates
                        .push(call.payload["update"].clone());
                    json!({"ok":true,"delivery":"notified"})
                }
                "notify_vigem_missing" => {
                    self.tray_vigem_notifications += 1;
                    json!({"ok":true,"delivery":"notified"})
                }
                _ => panic!("unexpected system tray action"),
            };
        }
        if service == "amaterasu.session-history/1" {
            return match action {
                "stream_activity_begin" => {
                    let id = call.payload["session_id"].as_str().unwrap().to_owned();
                    if self.history_storage_degraded {
                        json!({"ok":true,"reserved":false})
                    } else {
                        self.history_activities.insert(id);
                        json!({"ok":true,"reserved":true})
                    }
                }
                "stream_activity_end" => {
                    self.history_activity_ends.push(call.payload.clone());
                    if self.history_storage_degraded {
                        json!({"ok":false,"code":"storage_unavailable"})
                    } else {
                        let ended = self
                            .history_activities
                            .remove(call.payload["session_id"].as_str().unwrap());
                        json!({"ok":true,"ended":ended})
                    }
                }
                "session_begin" => {
                    self.history_begins.push(call.payload.clone());
                    if self.history_storage_degraded {
                        json!({"ok":false,"error":{"code":"storage_unavailable","message":"storage unavailable"}})
                    } else if self.history_recording_disabled {
                        json!({"ok":true,"recorded":false})
                    } else {
                        json!({"ok":true,"recorded":true})
                    }
                }
                "session_end" => {
                    self.history_ends.push(call.payload.clone());
                    if self.history_storage_degraded {
                        json!({"ok":false,"code":"storage_unavailable"})
                    } else {
                        json!({"ok":true,"recorded":true})
                    }
                }
                "record_sample" => {
                    self.history_samples.push(call.payload["sample"].clone());
                    if self.history_storage_degraded || self.history_failed_actions.contains(action)
                    {
                        json!({"ok":false,"error":{"code":"storage_unavailable","message":"storage unavailable"}})
                    } else {
                        json!({"ok":true,"recorded":!self.history_recording_disabled})
                    }
                }
                "record_event" => {
                    self.history_events.push(call.payload["event"].clone());
                    if self.history_storage_degraded || self.history_failed_actions.contains(action)
                    {
                        json!({"ok":false,"error":{"code":"storage_unavailable","message":"storage unavailable"}})
                    } else {
                        json!({"ok":true,"recorded":!self.history_recording_disabled})
                    }
                }
                _ => panic!("unexpected session history action"),
            };
        }
        if service == "amaterasu.input/1" {
            return self.input(action, &call.payload);
        }
        if service == "amaterasu.frame-limiter/1" {
            return self.frame_limiter(action, &call.payload);
        }
        if service == "amaterasu.display/1" {
            return self.display(action, &call.payload);
        }
        if matches!(action, "open" | "create") {
            if self.fail_open.as_deref() == Some(service) {
                return error("backend_failed");
            }
            let key = call.payload[if action == "create" {
                "key"
            } else {
                "operation_id"
            }]
            .as_str()
            .unwrap()
            .to_owned();
            if !self
                .allocations
                .contains_key(&(service.into(), key.clone()))
            {
                self.next += 1;
                self.allocations.insert(
                    (service.into(), key.clone()),
                    Allocation {
                        id: self.next,
                        key: key.clone(),
                        state: if service == "amaterasu.media-transport/1" {
                            "sending".into()
                        } else {
                            "preparing".into()
                        },
                        leases: Default::default(),
                        request: call.payload.clone(),
                    },
                );
            }
            let allocation = self.allocations[&(service.into(), key)].clone();
            return self.metadata(service, &allocation);
        }
        if action == "lookup" {
            let key = call.payload[if service == "amaterasu.session/1" {
                "key"
            } else {
                "operation_id"
            }]
            .as_str()
            .unwrap()
            .to_owned();
            if service == "amaterasu.video/1" && self.video_remaining != 0 {
                self.video_remaining -= 1;
                if self.video_remaining == 0 {
                    self.allocations.remove(&(service.into(), key.clone()));
                }
            }
            return self
                .allocations
                .get(&(service.into(), key))
                .map(|allocation| self.metadata(service, allocation))
                .unwrap_or_else(|| error("not_found"));
        }
        let id = call.payload[if service == "amaterasu.session/1" {
            "session_id"
        } else {
            "stream"
        }]
        .as_u64()
        .unwrap();
        let Some(key) = self
            .allocations
            .iter()
            .find(|((provider, _), value)| provider == service && value.id == id)
            .map(|(key, _)| key.clone())
        else {
            return error("not_found");
        };
        if action == "close" {
            if self.fail_close.as_deref() == Some(service) {
                return error("close_failed");
            }
            if service == "amaterasu.video/1" && self.video_close_polls > 0 {
                self.video_remaining = self.video_close_polls;
                self.allocations.get_mut(&key).unwrap().state = "closing".into();
            } else {
                self.allocations.remove(&key);
            }
            return json!({"ok":true,"stream":id,"state":"closing"});
        }
        if action == "recover" && service == "amaterasu.audio-encode/1" {
            if self.fail_audio_recover {
                return error("codec_unavailable");
            }
            return json!({"ok":true,"stream":if self.malformed_audio_recover {id + 1} else {id},"packet_bytes":160});
        }
        if service == "amaterasu.session/1" {
            let allocation = self.allocations.get_mut(&key).unwrap();
            match action {
                "acquire" => {
                    allocation.leases.insert(
                        call.payload["resource"].as_str().unwrap().into(),
                        json!("shared"),
                    );
                }
                "activate" => allocation.state = "active".into(),
                "stop" => allocation.state = "stopping".into(),
                "finish_stop" => {
                    allocation.state = "stopped".into();
                    allocation.leases.clear();
                }
                _ => panic!("unexpected session action"),
            }
            return self.metadata(service, &self.allocations[&key]);
        }
        if action == "status" && service == "amaterasu.control-transport/1" {
            return json!({"ok":true,"streams":[{"stream":id,"authenticated":self.authenticated,"state":if self.authenticated {"active"} else {"awaiting_authentication"}}]});
        }
        if action == "status" && service == "amaterasu.media-transport/1" {
            let mut metadata = self.metadata(service, &self.allocations[&key]);
            let status = metadata.as_object_mut().expect("media metadata");
            status.remove("ok");
            let discovery = self.allocations[&key].request["discovery"].is_object();
            let ready = !discovery
                || (self.discovered_video_port.is_some() && self.discovered_audio_port.is_some());
            status.insert(
                "state".into(),
                json!(if self.media_failure {
                    "failed"
                } else if !ready {
                    "discovering"
                } else if self.allocations.keys().any(|(provider, video_key)| {
                    provider == "amaterasu.video/1"
                        && key
                            .1
                            .strip_suffix("-media-transport")
                            .is_some_and(|prefix| video_key == &format!("{prefix}-video"))
                }) {
                    "drained"
                } else {
                    "sending"
                }),
            );
            status.insert(
                "failure".into(),
                if self.media_failure {
                    json!("udp_discovery_timeout")
                } else {
                    Value::Null
                },
            );
            status.insert("queued_datagrams".into(), json!(0));
            return json!({"ok":true,"streams":[metadata]});
        }
        json!({"ok":true,"stream":id})
    }

    fn display(&mut self, action: &str, payload: &Value) -> Value {
        if self.display_unavailable {
            return error("permission_denied");
        }
        match action {
            "status" => json!({"ok":true,"native_supported":true,"mutation_enabled":true,
            "state":if self.display_recovery_required {"recovery_required"} else if self.display_lease.is_some() {"leased"} else {"idle"},
            "owned_by_caller":self.display_lease.is_some(),"lease":self.display_lease,
            "operation_id":if self.display_wrong_status_operation && self.display_lease.is_some() {
                Some("another-session-display")
            } else {
                self.display_operation.as_deref()
            }}),
            "apply" => {
                if self.display_apply_failure {
                    return error("invalid_configuration");
                }
                if let Some(inactive) = self.display_inactive_target.as_deref() {
                    let Some(policy) = payload.get("mode_policy") else {
                        return error("unsupported_topology");
                    };
                    if policy["display_id"] != inactive
                        || policy["configuration_option"] != "ensure_active"
                        || payload.get("mode_policies").is_some()
                    {
                        return error("unsupported_topology");
                    }
                    let Some(displays) = payload["configuration"]["displays"].as_array() else {
                        return error("invalid_configuration");
                    };
                    let active: Vec<_> = displays
                        .iter()
                        .filter_map(|display| display["id"].as_str())
                        .filter(|id| *id != inactive)
                        .collect();
                    if active.len() != self.display_active_ids.len()
                        || !active.iter().all(|id| {
                            self.display_active_ids
                                .iter()
                                .any(|expected| expected == id)
                        })
                        || !displays.iter().any(|display| display["id"] == inactive)
                    {
                        return error("unsupported_topology");
                    }
                }
                assert_eq!(
                    payload["expected_topology_revision"],
                    "0123456789abcdef0123456789abcdef"
                );
                let operation = payload["operation_id"]
                    .as_str()
                    .expect("session operation ID");
                assert!(self.display_lease.is_none(), "duplicate display Apply");
                self.display_lease = Some("display-0000000000000001".into());
                self.display_operation = Some(operation.into());
                if self.display_apply_recovery_reply {
                    return json!({"ok":true,"applied":false,"state":"recovery_required",
                        "lease":self.display_lease,"operation_id":self.display_operation});
                }
                json!({"ok":true,"applied":true,"lease":self.display_lease,
                "operation_id":if self.display_wrong_apply_operation {
                    Some("another-session-display")
                } else {
                    self.display_operation.as_deref()
                }})
            }
            "restore" | "recover" => {
                if self.display_restore_failures > 0 {
                    self.display_restore_failures -= 1;
                    self.display_recovery_required = true;
                    return json!({"ok":false,"recovery_required":true,"lease":self.display_lease});
                }
                assert_eq!(payload["lease"].as_str(), self.display_lease.as_deref());
                let lease = self.display_lease.take();
                self.display_recovery_required = false;
                self.display_operation = None;
                json!({"ok":true,"restored":true,"lease":lease})
            }
            _ => panic!("unexpected display action"),
        }
    }

    fn frame_limiter(&mut self, action: &str, payload: &Value) -> Value {
        match action {
            "apply" => {
                if self.limiter_unavailable {
                    return json!({"ok":true,"version":1,"action":"apply",
                        "session_id":payload["session_id"],
                        "state":"runtime_unavailable","applied":false});
                }
                let session = payload["session_id"].as_str().unwrap();
                if self.limiter_session.as_deref() == Some(session) {
                    return json!({"ok":true,"version":1,"action":"apply",
                        "session_id":session,"state":"already_applied","applied":true,
                        "lease":self.limiter_lease});
                }
                let lease = format!("limiter-{session}");
                self.limiter_session = Some(session.into());
                self.limiter_lease = Some(lease.clone());
                self.limiter_executable_path =
                    payload["executable_path"].as_str().map(str::to_owned);
                json!({"ok":true,"version":1,"action":"apply","session_id":session,
                    "state":"applied","applied":true,"lease":lease})
            }
            "status" => {
                if self.limiter_malformed_status {
                    return json!({"ok":true,"version":1,"action":"status","state":"not_found",
                        "applied":false,"session_id":payload["session_id"]});
                }
                let leases = if self.limiter_session.as_deref() == payload["session_id"].as_str() {
                    vec![
                        json!({"session_id":self.limiter_session,"lease":self.limiter_lease,
                        "executable_path":self.limiter_status_executable_override.as_ref()
                            .or(self.limiter_executable_path.as_ref()),"provider":"rtss",
                        "state":"applied","applied":true}),
                    ]
                } else {
                    Vec::new()
                };
                json!({"ok":true,"version":1,"action":"status","session_id":payload["session_id"],
                    "state":if leases.is_empty() {"not_found"} else {"applied"},"applied":false,"leases":leases})
            }
            "restore" => {
                if self.limiter_restore_failures > 0 {
                    self.limiter_restore_failures -= 1;
                    return json!({"ok":true,"version":1,"action":"restore",
                        "state":"restore_failed","applied":true});
                }
                assert_eq!(payload["lease"], json!(self.limiter_lease));
                self.limiter_lease = None;
                self.limiter_session = None;
                self.limiter_executable_path = None;
                json!({"ok":true,"version":1,"action":"restore",
                    "state":"restored","applied":false})
            }
            _ => panic!("unexpected frame limiter action"),
        }
    }

    fn input(&mut self, action: &str, payload: &Value) -> Value {
        match action {
            "acquire" => {
                let operation = payload["operation_id"].as_str().unwrap();
                let ttl_ms = payload["ttl_ms"].as_u64().unwrap();
                if self.input_terminal && self.input_operation.as_deref() == Some(operation) {
                    return json!({"ok":true,"state":"released","operation_id":operation,"lease":Value::Null,"reason":"released"});
                }
                if let Some(lease) = &self.input_lease {
                    return if self.input_operation.as_deref() == Some(operation) {
                        self.input_active(operation, lease, ttl_ms)
                    } else {
                        json!({"ok":false,"error":{"code":"input_busy","message":"busy"}})
                    };
                }
                self.input_operation = Some(operation.to_owned());
                self.input_lease = Some("0123456789abcdef0123456789abcdef".into());
                self.input_ttl_ms = ttl_ms;
                self.input_terminal = false;
                self.input_active(
                    operation,
                    self.input_lease.as_deref().expect("input lease"),
                    ttl_ms,
                )
            }
            "lookup" => {
                let operation = payload["operation_id"].as_str().unwrap();
                if self.input_operation.as_deref() != Some(operation) {
                    return json!({"ok":false,"error":{"code":"not_found","message":"missing"}});
                }
                if let Some(lease) = &self.input_lease {
                    self.input_active(operation, lease, self.input_ttl_ms)
                } else {
                    json!({"ok":true,"state":"released","operation_id":operation,"lease":Value::Null,"reason":"released"})
                }
            }
            "renew" => {
                if payload["lease"] != json!(self.input_lease) {
                    return json!({"ok":false,"error":{"code":"permission_denied"}});
                }
                self.input_ttl_ms = payload["ttl_ms"].as_u64().unwrap();
                json!({"ok":true,"lease":self.input_lease,"ttl_ms":self.input_ttl_ms})
            }
            "batch" => {
                if payload["lease"] != json!(self.input_lease) {
                    return json!({"ok":false,"error":{"code":"permission_denied"}});
                }
                let events = payload["events"].as_array().unwrap();
                self.input_events.extend(events.iter().cloned());
                if self.fail_controller_input
                    && events.first().is_some_and(|event| {
                        matches!(
                            event["kind"].as_str(),
                            Some(
                                "controller"
                                    | "controller_arrival"
                                    | "controller_touch"
                                    | "controller_motion"
                                    | "controller_battery"
                            )
                        )
                    })
                {
                    self.fail_controller_input = false;
                    return json!({"ok":false,"lease":self.input_lease,
                        "accepted":events.len().saturating_sub(1),
                        "partial_event_possible":true,
                        "error":{"code":"controller_unavailable","message":"virtual gamepad unavailable"}});
                }
                if self.fail_input_batch {
                    self.input_lease = None;
                    self.input_terminal = true;
                    return json!({"ok":false,"error":{"code":"backend_failed"},"accepted":0,"partial_event_possible":true});
                }
                json!({"ok":true,"lease":self.input_lease,"accepted":events.len()})
            }
            "feedback_peek" => {
                assert_eq!(payload["lease"], json!(self.input_lease));
                json!({"ok":true,"events":self.input_feedback_events.iter().take(
                    payload["limit"].as_u64().unwrap_or(16) as usize
                ).cloned().collect::<Vec<_>>()})
            }
            "feedback_ack" => {
                assert_eq!(payload["lease"], json!(self.input_lease));
                let through = payload["through_sequence"].as_u64().unwrap();
                if self.feedback_ack_failures > 0 {
                    self.feedback_ack_failures -= 1;
                    return json!({"ok":false,"error":{"code":"temporarily_unavailable"}});
                }
                while self.input_feedback_events.front().is_some_and(|event| {
                    event["sequence"]
                        .as_u64()
                        .is_some_and(|sequence| sequence <= through)
                }) {
                    self.input_feedback_events.pop_front();
                }
                self.input_feedback_acks.push(through);
                json!({"ok":true,"through_sequence":through})
            }
            "release" => {
                if payload["lease"] != json!(self.input_lease) {
                    return json!({"ok":false,"error":{"code":"not_found"}});
                }
                self.input_lease = None;
                self.input_terminal = true;
                json!({"ok":true,"released":true})
            }
            _ => panic!("unexpected input action"),
        }
    }

    fn input_active(&self, operation: &str, lease: &str, ttl_ms: u64) -> Value {
        let mut reply = json!({
            "ok": true,
            "state": "active",
            "operation_id": operation,
            "lease": lease,
            "ttl_ms": ttl_ms,
            "expires_in_ms": self.input_expires_in_ms.unwrap_or(ttl_ms),
            "backend": "synthetic-test",
            "cleanup_required": self.input_cleanup_required,
        });
        if let Some(supported) = &self.input_absolute_pointer {
            reply["absolute_pointer"] = supported.clone();
        }
        if let Some(supported) = &self.input_controller_supported {
            reply["controller_input"] = supported.clone();
        }
        if let Some(supported) = &self.input_pen_touch_supported {
            reply["pen_touch_supported"] = supported.clone();
        }
        if let Some(enabled) = &self.input_native_pen_touch {
            reply["native_pen_touch"] = enabled.clone();
        }
        reply
    }

    fn metadata(&self, service: &str, allocation: &Allocation) -> Value {
        let id = allocation.id;
        match service {
            "amaterasu.session/1" => {
                json!({"ok":true,"sessions":[{"id":id,"key":allocation.key,"state":allocation.state,"leases":allocation.leases}]})
            }
            "amaterasu.audio-encode/1" => {
                let bitrate = allocation.request["bitrate"].as_u64().unwrap();
                let packet_ms = allocation.request["packet_ms"].as_u64().unwrap();
                let packet_bytes = bitrate * packet_ms / 8_000;
                let channels = allocation.request["channels"].as_u64().unwrap();
                let mut reply = json!({"ok":true,"stream":id,"sample_rate":48000,"channels":channels,
                    "packet_ms":packet_ms,"bitrate":bitrate,"packet_bytes":packet_bytes,"constant_bitrate":true});
                if let Some((streams, coupled, mapping, wave_mapping)) = (crate::model::AudioWire {
                    channels: channels as u32,
                    packet_ms: packet_ms as u32,
                    bitrate: bitrate as u32,
                    channel_mask: if channels == 6 { 0x3f } else { 0x63f },
                    layout: if channels == 6 { "5.1" } else { "7.1" }.into(),
                })
                .channel_mapping()
                {
                    reply["mapping_family"] = json!(1);
                    reply["streams"] = json!(streams);
                    reply["coupled_streams"] = json!(coupled);
                    reply["channel_mapping"] = json!(mapping);
                    reply["wave_channel_mapping"] = json!(wave_mapping);
                }
                reply
            }
            "amaterasu.media-transport/1" => {
                let request = &allocation.request;
                let video: crate::model::VideoWire =
                    serde_json::from_value(request["video"].clone())
                        .expect("video configuration in media open request");
                let mut metadata = json!({"ok":true,"stream":id,"peer_ip":request["peer_ip"],
                    "video_port":request["video_port"],"audio_port":request["audio_port"],
                    "video_local":"127.0.0.1:50001","audio_local":"127.0.0.1:50002",
                    "video":video.effective(),"video_batch_size_fallback":video.batch_size_fallback(),
                    "audio":request["audio"],
                    "stream_audio":request["stream_audio"],
                    "video_encrypted":request["encryption"]["video"].as_bool().unwrap_or(false),
                    "audio_encrypted":request["encryption"]["audio"].as_bool().unwrap_or(false),
                    "permit_legacy_audio_iv_wrap":request["encryption"]["permit_legacy_audio_iv_wrap"].as_bool().unwrap_or(false)});
                if request["discovery"].is_object() {
                    metadata["peer_discovery"] = json!(true);
                    metadata["video_port"] = json!(self.discovered_video_port);
                    metadata["audio_port"] = json!(self.discovered_audio_port);
                    metadata["ready"] = json!(
                        self.discovered_video_port.is_some()
                            && self.discovered_audio_port.is_some()
                    );
                }
                metadata
            }
            "amaterasu.control-transport/1" => {
                json!({"ok":true,"stream":id,"local":"127.0.0.1:50003"})
            }
            "amaterasu.video/1" => {
                let mut reply = json!({"ok":true,"stream":id,"state":if allocation.state=="closing" {"closing"} else {"running"},"packet_boundaries":true});
                if let Some(geometry) = &self.desktop_geometry {
                    reply["desktop_geometry"] = geometry.clone();
                }
                if let Some(port) = &self.touch_port {
                    reply["touch_port"] = port.clone();
                }
                reply
            }
            "amaterasu.audio/1" => {
                let channels = allocation.request["channels"].as_u64().unwrap_or(2);
                let mask = match channels {
                    6 => 0x3f,
                    8 => 0x63f,
                    _ => 3,
                };
                json!({"ok":true,"stream":id,"endpoint":"resolved-canonical-endpoint","format":{"sample_rate":self.format_rate,"channels":channels,"channel_mask":mask,"format":"f32"}})
            }
            _ => panic!("unexpected component"),
        }
    }
}

#[derive(Default)]
pub struct PlaneState {
    pub stopping: bool,
    pub ticks: usize,
    pub media_ticks: usize,
    pub media_ticks_when_stopped: Option<usize>,
    pub dropped: usize,
    pub failure: Option<&'static str>,
    pub controls: Vec<crate::plane::ControlRecord>,
    pub accepted_video_access_units: u64,
    pub accepted_media_payload_bytes: u64,
    pub observed_video_sequence_gaps: u64,
    pub feedback: Vec<amaterasu_gamestream::control::HostFeedback>,
    pub feedback_queue_full: usize,
}
struct FakePlane(Rc<RefCell<PlaneState>>);
impl Drive for FakePlane {
    fn recover_audio(&mut self) {
        self.0.borrow_mut().failure = None;
    }
    fn tick(&mut self, media_active: bool) -> Result<PlaneProgress, PlaneFailure> {
        let mut state = self.0.borrow_mut();
        state.ticks += 1;
        state.media_ticks += usize::from(media_active);
        let progress = PlaneProgress {
            controls: std::mem::take(&mut state.controls),
            accepted_video_access_units: std::mem::take(&mut state.accepted_video_access_units),
            accepted_media_payload_bytes: std::mem::take(&mut state.accepted_media_payload_bytes),
            observed_video_sequence_gaps: std::mem::take(&mut state.observed_video_sequence_gaps),
        };
        if let Some(code) = state.failure {
            return Err(PlaneFailure { code, progress });
        }
        Ok(progress)
    }
    fn stop(&mut self) {
        let mut state = self.0.borrow_mut();
        state.stopping = true;
        state.media_ticks_when_stopped = Some(state.media_ticks);
    }
    fn is_drained(&self) -> bool {
        let state = self.0.borrow();
        state.stopping
            && state
                .media_ticks_when_stopped
                .is_some_and(|at_stop| state.media_ticks > at_stop)
    }
    fn enqueue_feedback(
        &mut self,
        feedback: amaterasu_gamestream::control::HostFeedback,
    ) -> Result<crate::plane::FeedbackEnqueueOutcome, crate::plane::FeedbackEnqueueError> {
        let mut state = self.0.borrow_mut();
        if state.feedback_queue_full > 0 {
            state.feedback_queue_full -= 1;
            return Err(crate::plane::FeedbackEnqueueError::QueueFull);
        }
        state.feedback.push(feedback);
        Ok(crate::plane::FeedbackEnqueueOutcome::Queued)
    }
}
impl Drop for FakePlane {
    fn drop(&mut self) {
        self.0.borrow_mut().dropped += 1;
    }
}

pub struct Harness {
    pub pipeline: Pipeline,
    pub broker: Broker,
    pub plane: Rc<RefCell<PlaneState>>,
    pub now: u64,
}
impl Harness {
    pub fn new() -> Self {
        Self {
            pipeline: Pipeline::new(Settings {
                enabled: true,
                allowed_principals: vec!["owner".into()],
                ..Settings::default()
            })
            .unwrap(),
            broker: Broker::new(),
            plane: Rc::new(RefCell::new(PlaneState::default())),
            now: 0,
        }
    }
    pub fn tick(&mut self) {
        self.now += 1;
        let state = self.plane.clone();
        self.pipeline
            .tick_with(
                self.now,
                &mut |command| self.broker.send(command),
                &mut |_| Ok(Box::new(FakePlane(state.clone()))),
            )
            .unwrap();
    }
    pub fn response(&mut self, request: u64, payload: Value) -> Value {
        self.response_from(request, "owner", payload)
    }
    pub fn response_from(&mut self, request: u64, owner: &str, payload: Value) -> Value {
        self.broker.enqueue(request, owner, payload);
        for _ in 0..600 {
            self.tick();
            if let Some(value) = self.broker.replies.remove(&request) {
                return value;
            }
        }
        panic!("workflow did not complete");
    }
    pub fn idle(&mut self) {
        for _ in 0..150 {
            self.tick();
        }
    }
}

pub fn prepare() -> Value {
    json!({"action":"prepare","operation_id":"launch-one","peer_ip":"127.0.0.1","video_port":51001,"audio_port":51002,"key_hex":"00112233445566778899aabbccddeeff","key_id":7,"connect_data":12})
}
pub fn prepare_discovery() -> Value {
    json!({"action":"prepare","operation_id":"launch-discovery","peer_ip":"127.0.0.1",
        "discovery":{"video_ping_payload":"0123456789abcdef","audio_ping_payload":"fedcba9876543210"},
        "key_hex":"00112233445566778899aabbccddeeff","key_id":7,"connect_data":12})
}
pub fn start(id: u64) -> Value {
    json!({"action":"start","session":id,"configuration":{"capture_provider":"dxgi","output":"monitor-one","encoder":0,"configuration":{"width":1280,"height":720,"fps":60,"bitrate":10000000},"audio_endpoint":"default"}})
}
