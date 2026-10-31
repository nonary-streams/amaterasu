//! Linux connector ownership uses libvirtualdisplay's broker, never Windows leases.
mod compositor;
mod connectors;
mod journal;
mod layout;
mod observer;
mod worker;
use crate::{
    model::{
        Operation, Settings, Specification, VirtualDisplayLayout, failure, valid_identity,
        valid_reference_id,
    },
    scale::ScaleSetting,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::mpsc::{self, Receiver, SyncSender},
    thread::JoinHandle,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub owner: String,
    pub specification: Specification,
    pub output: String,
    #[serde(default)]
    layout: Option<layout::Layout>,
    #[serde(default)]
    references: Option<BTreeSet<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transition: Option<Transition>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Transition {
    operation_id: String,
    reference_id: String,
    borrow_id: String,
    old: Specification,
    desired: Specification,
    completed: bool,
}
impl Record {
    fn key(&self) -> (String, String) {
        (self.owner.clone(), self.specification.identity.clone())
    }
    fn valid(&self) -> bool {
        self.references.as_ref().is_none_or(|references| {
            references.len() <= 64
                && references
                    .iter()
                    .all(|reference| valid_reference_id(reference))
        }) && self.layout.as_ref().is_none_or(layout::Layout::valid)
            && !self.owner.is_empty()
            && self.owner.len() <= 128
            && !self.owner.chars().any(char::is_control)
            && self.specification.validate().is_ok()
            && self.transition.as_ref().is_none_or(|transition| {
                !transition.operation_id.is_empty()
                    && transition.operation_id.len() <= 128
                    && transition
                        .operation_id
                        .bytes()
                        .all(|b| b.is_ascii_graphic())
                    && valid_reference_id(&transition.reference_id)
                    && transition.borrow_id.len() == 64
                    && transition.borrow_id.bytes().all(|b| b.is_ascii_hexdigit())
                    && transition.old.validate().is_ok()
                    && transition.desired.validate().is_ok()
                    && transition.old.identity == self.specification.identity
                    && transition.desired.identity == self.specification.identity
                    && if transition.completed {
                        self.specification == transition.desired
                    } else {
                        self.specification == transition.old
                    }
            })
            && connectors::connector_id(&self.output).is_some()
    }
}
#[derive(Clone)]
struct Monitor {
    specification: Specification,
    state: &'static str,
    output: Option<String>,
    error: Option<String>,
    references: Option<BTreeSet<String>>,
    layout: VirtualDisplayLayout,
    scale: ScaleSetting,
}
impl Monitor {
    fn response(&self, reference_id: Option<&str>) -> Value {
        json!({"reference_owned":reference_id.is_some_and(|reference|self.references.as_ref().is_some_and(|references|references.contains(reference))),"specification":self.specification,"virtual_display_layout":self.layout,"dd_virtual_display_scale":Manager::scale_value(self.scale),"state":self.state,"output":self.output,"error":self.error,
            "capture_output":self.output.as_ref().filter(|_|self.state=="ready").map(|output|format!("kwin:{output}")),
            "ready":self.output.as_ref().filter(|_|self.state=="ready").map(|output|json!({"device_path":output,"active":true}))})
    }
}
pub struct Admission {
    key: (String, String),
    reference_id: Option<String>,
    borrow_id: Option<String>,
}
struct Borrow {
    key: (String, String),
    reference_id: String,
    observation: observer::Observation,
    valid: bool,
}
struct Reconfiguration {
    transition: Transition,
    state: &'static str,
    result: Option<Value>,
    domain_id: String,
    revision: u64,
}
enum Work {
    Reconfigure {
        key: (String, String),
        transition: Transition,
        scale: ScaleSetting,
        layout: VirtualDisplayLayout,
    },
    Create {
        owner: String,
        specification: Specification,
        scale: ScaleSetting,
        references: Option<BTreeSet<String>>,
        layout: VirtualDisplayLayout,
    },
    References {
        key: (String, String),
        references: BTreeSet<String>,
        reply: SyncSender<Result<(), String>>,
    },
    Remove {
        key: (String, String),
    },
}
enum Event {
    Reconfigured {
        key: (String, String),
        result: Result<Record, String>,
    },
    Recovered {
        records: Vec<Record>,
        error: Option<String>,
    },
    Created {
        key: (String, String),
        result: Result<Record, String>,
    },
    Removed {
        key: (String, String),
        result: Result<(), String>,
    },
}

pub struct Manager {
    settings: Settings,
    monitors: BTreeMap<(String, String), Monitor>,
    ready: bool,
    recovery_error: Option<String>,
    sender: Option<SyncSender<Work>>,
    receiver: Option<Receiver<Event>>,
    worker: Option<JoinHandle<()>>,
    cleanup_pending: std::collections::BTreeSet<(String, String)>,
    observer: Option<observer::Observer>,
    borrows: BTreeMap<String, Borrow>,
    reconfigurations: BTreeMap<(String, String), Reconfiguration>,
    domain_id: String,
    topology_revision: u64,
    pending_mutations: usize,
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl Manager {
    pub fn start(settings: Settings) -> Result<Self, String> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| "display domain identity unavailable")?;
        let domain_id = format!(
            "provider_session:{}",
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let mut manager = Self {
            settings,
            monitors: BTreeMap::new(),
            ready: false,
            recovery_error: None,
            sender: None,
            receiver: None,
            worker: None,
            cleanup_pending: Default::default(),
            observer: None,
            borrows: BTreeMap::new(),
            reconfigurations: BTreeMap::new(),
            domain_id,
            topology_revision: 0,
            pending_mutations: 0,
            stopped: Default::default(),
        };
        if manager.settings.enabled {
            let journal = journal::Journal::for_user()?;
            let (sender, jobs) = mpsc::sync_channel(16);
            let (events, receiver) = mpsc::sync_channel(16);
            let stopped = manager.stopped.clone();
            let layout = manager.settings.virtual_display_layout;
            let worker = std::thread::Builder::new()
                .name("amaterasu-displays".into())
                .spawn(move || {
                    worker::run(
                        journal,
                        crate::linux_control::LinuxControlClient::default(),
                        compositor::KScreen,
                        connectors::SysfsConnectors::default(),
                        layout,
                        jobs,
                        events,
                        stopped,
                    );
                })
                .map_err(|e| e.to_string())?;
            manager.sender = Some(sender);
            manager.receiver = Some(receiver);
            manager.worker = Some(worker);
        }
        Ok(manager)
    }
    pub fn tick(&mut self) {
        while let Some(event) = self
            .receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
        {
            match event {
                Event::Reconfigured { key, result } => {
                    self.pending_mutations = self.pending_mutations.saturating_sub(1);
                    if let Some(change) = self.reconfigurations.get_mut(&key) {
                        match result {
                            Ok(record) => {
                                if let Some(monitor) = self.monitors.get_mut(&key) {
                                    monitor.specification = record.specification;
                                    monitor.state = "ready";
                                }
                                change.state = "observing";
                            }
                            Err(_) => {
                                change.state = "recovery_required";
                                if let Some(monitor) = self.monitors.get_mut(&key) {
                                    monitor.state = "cleanup_failed";
                                }
                            }
                        }
                    }
                }
                Event::Recovered { records, error } => {
                    self.ready = error.is_none();
                    self.recovery_error = error;
                    for record in records {
                        self.monitors.insert(
                            record.key(),
                            Monitor {
                                specification: record.specification,
                                state: "cleanup_failed",
                                output: Some(record.output),
                                error: Some("restart_cleanup_failed".into()),
                                references: record.references,
                                layout: self.settings.virtual_display_layout,
                                scale: self.settings.dd_virtual_display_scale,
                            },
                        );
                    }
                }
                Event::Created { key, result } => {
                    self.pending_mutations = self.pending_mutations.saturating_sub(1);
                    if let Some(monitor) = self.monitors.get_mut(&key) {
                        match result {
                            Ok(record) => {
                                monitor.output = Some(record.output);
                                monitor.state = "ready";
                            }
                            Err(error) => {
                                monitor.error = Some(error);
                                monitor.state = "cleanup_failed";
                            }
                        }
                        if self.cleanup_pending.contains(&key) {
                            monitor.state = "removing";
                        }
                    }
                }
                Event::Removed { key, result } => {
                    self.pending_mutations = self.pending_mutations.saturating_sub(1);
                    self.cleanup_pending.remove(&key);
                    match result {
                        Ok(()) => {
                            self.monitors.remove(&key);
                            self.reconfigurations.remove(&key);
                            if self.monitors.is_empty() && self.recovery_error.is_some() {
                                self.ready = true;
                                self.recovery_error = None;
                            }
                        }
                        Err(error) => {
                            if let Some(monitor) = self.monitors.get_mut(&key) {
                                monitor.state = "cleanup_failed";
                                monitor.error = Some(error);
                            }
                        }
                    }
                }
            }
        }
    }
    pub fn admit(&mut self, caller: &str, operation: Operation) -> (Value, Option<Admission>) {
        self.tick();
        match operation {
            Operation::ReconfigureStatus {
                operation_id,
                identity,
                reference_id,
                borrow_id,
                expected_topology_domain_id,
                expected_topology_revision,
            } => (
                self.reconfigure_status(
                    caller,
                    &operation_id,
                    &identity,
                    &reference_id,
                    &borrow_id,
                    &expected_topology_domain_id,
                    expected_topology_revision,
                ),
                None,
            ),
            Operation::Reconfigure {
                operation_id,
                identity,
                reference_id,
                borrow_id,
                width,
                height,
                refresh_millihz,
                expected_topology_domain_id,
                expected_topology_revision,
            } => (
                self.reconfigure(
                    caller,
                    operation_id,
                    identity,
                    reference_id,
                    borrow_id,
                    width,
                    height,
                    refresh_millihz,
                    expected_topology_domain_id,
                    expected_topology_revision,
                ),
                None,
            ),
            Operation::Borrow {
                identity,
                reference_id,
            } => self.borrow(caller, &identity, &reference_id),
            Operation::ValidateBorrow {
                identity,
                reference_id,
                borrow_id,
            } => (
                self.validate_borrow(caller, &identity, &reference_id, &borrow_id),
                None,
            ),
            Operation::ReleaseBorrow {
                identity,
                reference_id,
                borrow_id,
            } => (
                self.release_borrow(caller, &identity, &reference_id, &borrow_id),
                None,
            ),
            Operation::Status {
                identity,
                reference_id,
            } => (self.status(caller, identity, reference_id), None),
            Operation::Remove {
                identity,
                reference_id,
            } => (
                self.remove(caller, &identity, reference_id.as_deref()),
                None,
            ),
            Operation::Create {
                hdr_enabled,
                identity,
                width,
                height,
                refresh_millihz,
                name,
                dd_virtual_display_scale,
                reference_id,
                virtual_display_layout,
                expected_topology_revision,
                expected_topology_domain_id,
            } => {
                if self
                    .reconfigurations
                    .values()
                    .any(|change| change.state != "succeeded")
                {
                    return (failure("reconfigure_pending"), None);
                }
                match (&expected_topology_domain_id, expected_topology_revision) {
                    (None, None) => {}
                    (Some(domain), Some(revision))
                        if domain == &self.domain_id && revision == self.topology_revision => {}
                    (Some(_), Some(_)) => return (failure("topology_changed"), None),
                    _ => return (failure("invalid_request"), None),
                }
                let layout = virtual_display_layout.unwrap_or(self.settings.virtual_display_layout);
                if reference_id
                    .as_deref()
                    .is_some_and(|reference| !valid_reference_id(reference))
                {
                    return (failure("invalid_request"), None);
                }
                let scale = match dd_virtual_display_scale.map(ScaleSetting::try_from) {
                    None => self.settings.dd_virtual_display_scale,
                    Some(Ok(scale)) => scale,
                    Some(Err(())) => return (failure("invalid_request"), None),
                };
                if !self.settings.enabled {
                    return (failure("disabled"), None);
                }
                if !self.settings.dd_use_sunshine_virtual_display_driver {
                    return (failure("unsupported_driver"), None);
                }
                if !self.ready {
                    return (failure("startup_reconciliation_pending"), None);
                }
                let specification = Specification {
                    hdr_enabled,
                    identity,
                    width,
                    height,
                    refresh_millihz,
                    name,
                };
                if specification.validate().is_err() {
                    return (failure("invalid_request"), None);
                }
                let key = (caller.to_owned(), specification.identity.clone());
                if let Some(existing) = self.monitors.get(&key) {
                    if existing.specification != specification
                        || existing.layout != layout
                        || existing.scale != scale
                    {
                        return (failure("identity_conflict"), None);
                    }
                    let Some(reference) = reference_id.as_ref() else {
                        return (
                            if existing.references.is_none() {
                                json!({"ok":true,"monitor":existing.response(None)})
                            } else {
                                failure("reference_conflict")
                            },
                            None,
                        );
                    };
                    let Some(references) = existing.references.as_ref() else {
                        return (failure("reference_conflict"), None);
                    };
                    if references.contains(reference) {
                        return (
                            json!({"ok":true,"monitor":existing.response(Some(reference)),"reference_owned":true}),
                            None,
                        );
                    }
                    if matches!(existing.state, "removing" | "cleanup_failed") {
                        return (failure("cleanup_pending"), None);
                    }
                    if references.len() >= 64 {
                        return (failure("reference_limit"), None);
                    }
                    let mut references = references.clone();
                    references.insert(reference.clone());
                    if !self.persist_references(&key, references) {
                        return (failure("journal_unavailable"), None);
                    }
                    return (
                        json!({"ok":true,"monitor":self.monitors[&key].response(Some(reference)),"reference_owned":true}),
                        Some(Admission {
                            key,
                            reference_id,
                            borrow_id: None,
                        }),
                    );
                }
                if !self.borrows.is_empty() {
                    return (failure("topology_borrowed"), None);
                }
                if self.topology_revision == u64::MAX {
                    return (failure("topology_revision_exhausted"), None);
                }
                if self.monitors.len() >= 8 {
                    return (failure("resource_exhausted"), None);
                }
                let references = reference_id
                    .as_ref()
                    .map(|reference| BTreeSet::from([reference.clone()]));
                if !self.enqueue(Work::Create {
                    owner: caller.into(),
                    specification: specification.clone(),
                    scale,
                    references: references.clone(),
                    layout,
                }) {
                    return (failure("busy"), None);
                }
                self.topology_revision += 1;
                self.pending_mutations += 1;
                let monitor = Monitor {
                    specification,
                    state: "creating",
                    output: None,
                    error: None,
                    references,
                    layout,
                    scale,
                };
                let response = json!({"ok":true,"monitor":monitor.response(reference_id.as_deref()),"reference_owned":reference_id.is_some()});
                self.monitors.insert(key.clone(), monitor);
                (
                    response,
                    Some(Admission {
                        key,
                        reference_id,
                        borrow_id: None,
                    }),
                )
            }
        }
    }
    pub fn completion_lost(&mut self, admission: Option<Admission>) {
        if let Some(admission) = admission {
            if let Some(borrow_id) = admission.borrow_id {
                if self.borrows.remove(&borrow_id).is_some() {
                    self.topology_revision = self.topology_revision.saturating_add(1);
                }
                return;
            }
            let _ = self.remove(
                &admission.key.0,
                &admission.key.1,
                admission.reference_id.as_deref(),
            );
        }
    }
    fn enqueue(&self, work: Work) -> bool {
        self.sender
            .as_ref()
            .is_some_and(|sender| sender.try_send(work).is_ok())
    }
    fn persist_references(&mut self, key: &(String, String), references: BTreeSet<String>) -> bool {
        // The actor serializes updates behind creation and acknowledges the durable
        // journal replacement before this caller can observe ownership success.
        let (reply, result) = mpsc::sync_channel(1);
        if !self.enqueue(Work::References {
            key: key.clone(),
            references: references.clone(),
            reply,
        }) {
            return false;
        }
        if !matches!(result.recv(), Ok(Ok(()))) {
            return false;
        }
        self.monitors
            .get_mut(key)
            .expect("owned monitor")
            .references = Some(references);
        true
    }
    fn remove(&mut self, caller: &str, identity: &str, reference_id: Option<&str>) -> Value {
        if self
            .reconfigurations
            .values()
            .any(|change| change.state == "pending")
        {
            return failure("reconfigure_pending");
        }
        if !valid_identity(identity)
            || reference_id.is_some_and(|reference| !valid_reference_id(reference))
        {
            return failure("invalid_request");
        }
        let key = (caller.to_owned(), identity.to_owned());
        if !self.ready && self.recovery_error.is_none() {
            return failure("startup_reconciliation_pending");
        }
        if self.borrows.values().any(|borrow| {
            borrow.key == key
                && reference_id.is_none_or(|reference| borrow.reference_id == reference)
        }) {
            return failure("display_borrowed");
        }
        if !self.monitors.contains_key(&key) {
            return json!({"ok":true,"removed":true,"identity":identity});
        }
        let removes_output = reference_id.is_none_or(|reference| {
            self.monitors[&key]
                .references
                .as_ref()
                .is_none_or(|refs| refs.is_empty() || (refs.len() == 1 && refs.contains(reference)))
        });
        if removes_output && !self.borrows.is_empty() {
            return failure("topology_borrowed");
        }
        if removes_output && self.topology_revision == u64::MAX {
            return failure("topology_revision_exhausted");
        }
        if let Some(reference) = reference_id {
            let Some(references) = self.monitors[&key].references.as_ref() else {
                return failure("reference_conflict");
            };
            let mut references = references.clone();
            let released = references.remove(reference);
            let last = references.is_empty();
            if !self.persist_references(&key, references) {
                return failure("journal_unavailable");
            }
            if !last {
                return json!({"ok":true,"removed":false,"identity":identity,"reference_released":released,"reference_owned":false});
            }
        } else if self.monitors[&key].references.is_some()
            && !self.persist_references(&key, BTreeSet::new())
        {
            return failure("journal_unavailable");
        }
        if !self.cleanup_pending.contains(&key) {
            if !self.enqueue(Work::Remove { key: key.clone() }) {
                return failure("busy");
            }
            self.topology_revision += 1;
            self.pending_mutations += 1;
            self.cleanup_pending.insert(key.clone());
            self.monitors.get_mut(&key).unwrap().state = "removing";
        }
        json!({"ok":true,"removed":false,"monitor":self.monitors[&key].response(reference_id),"reference_owned":false})
    }
    /// Lookup only: never enqueue work or admit an unknown operation. The
    /// observing phase may finish the already completed operation's borrow.
    #[allow(clippy::too_many_arguments)]
    fn reconfigure_status(
        &mut self,
        caller: &str,
        operation_id: &str,
        identity: &str,
        reference_id: &str,
        borrow_id: &str,
        domain_id: &str,
        revision: u64,
    ) -> Value {
        let key = (caller.to_owned(), identity.to_owned());
        let Some(change) = self.reconfigurations.get(&key) else {
            return failure("recovery_required");
        };
        if self.domain_id != domain_id
            || change.domain_id != domain_id
            || change.revision != revision
            || change.transition.operation_id != operation_id
            || change.transition.reference_id != reference_id
            || change.transition.borrow_id != borrow_id
        {
            return failure("recovery_required");
        }
        if let Some(result) = change.result.clone() {
            let Some(new_borrow) = result["borrow_id"].as_str() else {
                return failure("recovery_required");
            };
            let verified = self.validate_borrow(caller, identity, reference_id, new_borrow);
            return if verified["ok"] == true
                && verified["mutation_domain"] == result["mutation_domain"]
            {
                result
            } else {
                failure("recovery_required")
            };
        }
        match change.state {
            "pending" => json!({"ok":true,"state":"pending","operation_id":operation_id}),
            "observing" => self.finish_reconfigure(&key),
            _ => failure("recovery_required"),
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn reconfigure(
        &mut self,
        caller: &str,
        operation_id: String,
        identity: String,
        reference_id: String,
        borrow_id: String,
        width: u32,
        height: u32,
        refresh_millihz: u32,
        domain_id: String,
        revision: u64,
    ) -> Value {
        let key = (caller.to_owned(), identity.clone());
        if let Some(change) = self.reconfigurations.get(&key)
            && !(change.state == "succeeded" && change.transition.operation_id != operation_id)
        {
            let t = &change.transition;
            if t.operation_id != operation_id
                || t.reference_id != reference_id
                || t.borrow_id != borrow_id
                || t.desired.width != width
                || t.desired.height != height
                || t.desired.refresh_millihz != refresh_millihz
                || change.domain_id != domain_id
                || change.revision != revision
            {
                return failure("reconfigure_conflict");
            }
            if let Some(result) = &change.result {
                let result = result.clone();
                let Some(new_borrow) = result["borrow_id"].as_str() else {
                    return failure("recovery_required");
                };
                let validation = self.validate_borrow(caller, &identity, &reference_id, new_borrow);
                if validation["ok"] != true
                    || validation["mutation_domain"] != result["mutation_domain"]
                {
                    return failure("recovery_required");
                }
                return result;
            }
            match change.state {
                "pending" => {
                    return json!({"ok":true,"state":"pending","operation_id":operation_id});
                }
                "recovery_required" => return failure("recovery_required"),
                "observing" => return self.finish_reconfigure(&key),
                _ => return failure("recovery_required"),
            }
        }
        if operation_id.is_empty()
            || operation_id.len() > 128
            || !operation_id.bytes().all(|b| b.is_ascii_graphic())
            || domain_id != self.domain_id
            || revision != self.topology_revision
            || self.pending_mutations != 0
            || !self.ready
            || self.borrows.len() != 1
            || self.topology_revision == u64::MAX
        {
            return failure("reconfigure_precondition_failed");
        }
        if self.validate_borrow(caller, &identity, &reference_id, &borrow_id)["ok"] != true {
            return failure("borrow_invalid");
        }
        let Some(monitor) = self.monitors.get(&key) else {
            return failure("not_found");
        };
        if monitor.state != "ready"
            || !monitor
                .references
                .as_ref()
                .is_some_and(|refs| refs.len() == 1 && refs.contains(&reference_id))
        {
            return failure("reference_conflict");
        }
        let mut desired = monitor.specification.clone();
        desired.width = width;
        desired.height = height;
        desired.refresh_millihz = refresh_millihz;
        if desired.validate().is_err() {
            return failure("invalid_request");
        }
        let transition = Transition {
            operation_id: operation_id.clone(),
            reference_id,
            borrow_id: borrow_id.clone(),
            old: monitor.specification.clone(),
            desired,
            completed: false,
        };
        let work = Work::Reconfigure {
            key: key.clone(),
            transition: transition.clone(),
            scale: monitor.scale,
            layout: monitor.layout,
        };
        if !self.enqueue(work) {
            return failure("busy");
        }
        self.borrows
            .get_mut(&borrow_id)
            .expect("verified borrow")
            .valid = false;
        self.monitors.get_mut(&key).expect("verified monitor").state = "reconfiguring";
        self.topology_revision += 1;
        self.pending_mutations += 1;
        self.reconfigurations.insert(
            key,
            Reconfiguration {
                transition,
                state: "pending",
                result: None,
                domain_id,
                revision,
            },
        );
        json!({"ok":true,"state":"pending","operation_id":operation_id})
    }
    fn finish_reconfigure(&mut self, key: &(String, String)) -> Value {
        let Some(change) = self.reconfigurations.get(key) else {
            return failure("recovery_required");
        };
        let old_borrow = change.transition.borrow_id.clone();
        let reference_id = change.transition.reference_id.clone();
        let operation_id = change.transition.operation_id.clone();
        let Some(old_observation) = self
            .borrows
            .get(&old_borrow)
            .map(|borrow| borrow.observation)
        else {
            return failure("recovery_required");
        };
        let Some(monitor) = self.monitors.get(key) else {
            return failure("recovery_required");
        };
        let Some(output) = monitor.output.clone() else {
            return failure("recovery_required");
        };
        let desired = monitor.specification.clone();
        if !matches!(
            crate::linux_control::LinuxControlClient::default().query_connector(&output),
            Ok(true)
        ) {
            return failure("recovery_required");
        }
        let Some(observation) = self
            .observer
            .as_mut()
            .and_then(|observer| observer.read(&output))
        else {
            return failure("recovery_required");
        };
        if !same_output_identity(old_observation, observation)
            || observation.width != desired.width
            || observation.height != desired.height
            || observation.refresh != desired.refresh_millihz
        {
            return failure("recovery_required");
        }
        if self.topology_revision == u64::MAX {
            return failure("recovery_required");
        }
        let mut random = [0u8; 32];
        if getrandom::fill(&mut random).is_err() {
            return failure("recovery_required");
        }
        let new_borrow: String = random.iter().map(|b| format!("{b:02x}")).collect();
        self.borrows.remove(&old_borrow);
        self.borrows.insert(
            new_borrow.clone(),
            Borrow {
                key: key.clone(),
                reference_id,
                observation,
                valid: true,
            },
        );
        self.topology_revision += 1;
        self.reconfigurations
            .get_mut(key)
            .expect("existing transition")
            .state = "succeeded";
        let result = json!({"ok":true,"state":"succeeded","operation_id":operation_id,"borrow_id":new_borrow,"observed_output":Self::observation(&output, observation),"mutation_domain":self.domain()});
        self.reconfigurations
            .get_mut(key)
            .expect("existing transition")
            .result = Some(result.clone());
        result
    }
    fn borrow(
        &mut self,
        caller: &str,
        identity: &str,
        reference_id: &str,
    ) -> (Value, Option<Admission>) {
        if self
            .reconfigurations
            .values()
            .any(|change| change.state != "succeeded")
        {
            return (failure("reconfigure_pending"), None);
        }
        let key = (caller.to_owned(), identity.to_owned());
        if self.pending_mutations != 0 || !self.ready {
            return (failure("topology_mutating"), None);
        }
        if self.topology_revision == u64::MAX {
            return (failure("topology_revision_exhausted"), None);
        }
        let Some(monitor) = self.monitors.get(&key) else {
            return (failure("not_found"), None);
        };
        if monitor.state != "ready"
            || !monitor
                .references
                .as_ref()
                .is_some_and(|refs| refs.contains(reference_id))
        {
            return (failure("reference_not_owned"), None);
        }
        let Some(output) = monitor.output.clone() else {
            return (failure("binding_unavailable"), None);
        };
        if self.borrows.len() >= 64 {
            return (failure("resource_exhausted"), None);
        }
        if !matches!(
            crate::linux_control::LinuxControlClient::default().query_connector(&output),
            Ok(true)
        ) {
            return (failure("binding_unavailable"), None);
        }
        if self.observer.is_none() {
            self.observer = observer::Observer::open();
        }
        let Some(observation) = self
            .observer
            .as_mut()
            .and_then(|observer| observer.read(&output))
        else {
            self.invalidate_observer();
            return (failure("binding_unavailable"), None);
        };
        if observation.width != monitor.specification.width
            || observation.height != monitor.specification.height
            || observation.refresh != monitor.specification.refresh_millihz
        {
            return (failure("binding_mismatch"), None);
        }
        let mut random = [0u8; 32];
        if getrandom::fill(&mut random).is_err() {
            return (failure("random_unavailable"), None);
        }
        let borrow_id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        self.topology_revision += 1;
        self.borrows.insert(
            borrow_id.clone(),
            Borrow {
                key: key.clone(),
                reference_id: reference_id.into(),
                observation,
                valid: true,
            },
        );
        let result = json!({"ok":true,"borrow_id":borrow_id,"observed_output":Self::observation(&output, observation),"mutation_domain":self.domain()});
        (
            result,
            Some(Admission {
                key,
                reference_id: Some(reference_id.into()),
                borrow_id: Some(borrow_id),
            }),
        )
    }
    fn invalidate_observer(&mut self) {
        self.observer = None;
        for borrow in self.borrows.values_mut() {
            borrow.valid = false;
        }
    }
    fn observation(output: &str, observation: observer::Observation) -> Value {
        let compositor_id: String = observation
            .compositor_id
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        json!({"version":1,"protocol":"wayland-wl-output","compositor_id":compositor_id,"registry_global":observation.global,"output_name":output,"width":observation.width,"height":observation.height,"refresh_millihz":observation.refresh})
    }
    fn validate_borrow(
        &mut self,
        caller: &str,
        identity: &str,
        reference_id: &str,
        borrow_id: &str,
    ) -> Value {
        let key = (caller.to_owned(), identity.to_owned());
        let Some(borrow) = self.borrows.get(borrow_id) else {
            return failure("borrow_invalid");
        };
        if borrow.key != key || borrow.reference_id != reference_id || !borrow.valid {
            return failure("borrow_invalid");
        }
        let expected = borrow.observation;
        let output = self.monitors.get(&key).and_then(|monitor| {
            (monitor.state == "ready"
                && monitor
                    .references
                    .as_ref()
                    .is_some_and(|refs| refs.contains(reference_id)))
            .then(|| monitor.output.clone())
            .flatten()
        });
        let Some(output) = output else {
            self.borrows
                .get_mut(borrow_id)
                .expect("borrow exists")
                .valid = false;
            return failure("borrow_invalid");
        };
        let connected = matches!(
            crate::linux_control::LinuxControlClient::default().query_connector(&output),
            Ok(true)
        );
        let observed = self
            .observer
            .as_mut()
            .and_then(|observer| observer.read(&output));
        if !connected || observed != Some(expected) {
            self.borrows
                .get_mut(borrow_id)
                .expect("borrow exists")
                .valid = false;
            return failure("borrow_invalid");
        }
        json!({"ok":true,"borrow_id":borrow_id,"observed_output":Self::observation(&output, expected),"mutation_domain":self.domain()})
    }
    fn release_borrow(
        &mut self,
        caller: &str,
        identity: &str,
        reference_id: &str,
        borrow_id: &str,
    ) -> Value {
        if self
            .reconfigurations
            .get(&(caller.to_owned(), identity.to_owned()))
            .is_some_and(|change| {
                change.transition.borrow_id == borrow_id && change.state == "pending"
            })
        {
            return failure("reconfigure_pending");
        }
        if self.borrows.get(borrow_id).is_some_and(|borrow| {
            borrow.key == (caller.to_owned(), identity.to_owned())
                && borrow.reference_id == reference_id
        }) {
            self.borrows.remove(borrow_id);
            self.topology_revision = self.topology_revision.saturating_add(1);
            json!({"ok":true,"released":true,"mutation_domain":self.domain()})
        } else {
            failure("borrow_invalid")
        }
    }
    fn scale_value(scale: ScaleSetting) -> i32 {
        match scale {
            ScaleSetting::Retain => 0,
            ScaleSetting::Automatic => -1,
            ScaleSetting::Fixed(value) => i32::from(value),
        }
    }
    fn domain(&self) -> Value {
        let unresolved = self
            .reconfigurations
            .values()
            .any(|change| change.state != "succeeded");
        let pending = self.pending_mutations.max(usize::from(unresolved));
        json!({"id":self.domain_id,"revision":self.topology_revision,"borrowers":self.borrows.len(),"pending_mutations":pending,"scope":"provider_session"})
    }
    fn status(
        &self,
        caller: &str,
        identity: Option<String>,
        reference_id: Option<String>,
    ) -> Value {
        if identity.as_deref().is_some_and(|id| !valid_identity(id))
            || (reference_id.is_some() && identity.is_none())
            || reference_id
                .as_deref()
                .is_some_and(|reference| !valid_reference_id(reference))
        {
            return failure("invalid_request");
        }
        let monitors: Vec<_> = self
            .monitors
            .iter()
            .filter(|((owner, id), _)| {
                owner == caller && identity.as_ref().is_none_or(|wanted| wanted == id)
            })
            .map(|(_, monitor)| monitor.response(reference_id.as_deref()))
            .collect();
        let reference_owned = identity
            .as_ref()
            .and_then(|identity| self.monitors.get(&(caller.to_owned(), identity.clone())))
            .is_some_and(|monitor| {
                reference_id.as_ref().is_some_and(|reference| {
                    monitor
                        .references
                        .as_ref()
                        .is_some_and(|references| references.contains(reference))
                })
            });
        json!({"reference_owned":reference_id.map(|_|reference_owned),"ok":true,"enabled":self.settings.enabled,"native_supported":true,"driver":"libvirtualdisplay",
            "ownership_authority":"unix_uid_broker","mutation_domain":self.domain(),"settings":{"virtual_display_layout":self.settings.virtual_display_layout,"dd_virtual_display_scale":Self::scale_value(self.settings.dd_virtual_display_scale),"dd_activate_virtual_display":self.settings.dd_activate_virtual_display,"dd_use_sunshine_virtual_display_driver":self.settings.dd_use_sunshine_virtual_display_driver},"recovery_ready":self.ready && self.reconfigurations.values().all(|change| change.state == "succeeded"),"recovery_error":self.recovery_error,"monitors":monitors,
            "activation_requested":self.settings.dd_activate_virtual_display,
            "configured_permanent_monitor_count":self.settings.dd_virtual_display_permanent_count,"permanent_monitor_runtime_supported":false})
    }
}
fn same_output_identity(before: observer::Observation, after: observer::Observation) -> bool {
    before.compositor_id == after.compositor_id && before.global == after.global
}
impl Drop for Manager {
    fn drop(&mut self) {
        // The actor owns native mutations and its journal until shutdown finishes.
        // Closing the event receiver prevents a full queue from blocking its join.
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        self.sender.take();
        self.receiver.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
