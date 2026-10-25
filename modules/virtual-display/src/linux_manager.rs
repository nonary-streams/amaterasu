//! Linux connector ownership uses libvirtualdisplay's broker, never Windows leases.
mod compositor;
mod connectors;
mod journal;
mod layout;
mod worker;
use crate::{
    model::{Operation, Settings, Specification, failure, valid_identity},
    scale::ScaleSetting,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
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
}
impl Record {
    fn key(&self) -> (String, String) {
        (self.owner.clone(), self.specification.identity.clone())
    }
    fn valid(&self) -> bool {
        self.layout.as_ref().is_none_or(layout::Layout::valid)
            && !self.owner.is_empty()
            && self.owner.len() <= 128
            && !self.owner.chars().any(char::is_control)
            && self.specification.validate().is_ok()
            && connectors::connector_id(&self.output).is_some()
    }
}
#[derive(Clone)]
struct Monitor {
    specification: Specification,
    state: &'static str,
    output: Option<String>,
    error: Option<String>,
}
impl Monitor {
    fn response(&self) -> Value {
        json!({"specification":self.specification,"state":self.state,"output":self.output,"error":self.error,
            "capture_output":self.output.as_ref().filter(|_|self.state=="ready").map(|output|format!("kwin:{output}")),
            "ready":self.output.as_ref().filter(|_|self.state=="ready").map(|output|json!({"device_path":output,"active":true}))})
    }
}
pub struct Admission {
    key: (String, String),
}
enum Work {
    Create {
        owner: String,
        specification: Specification,
    },
    Remove {
        key: (String, String),
    },
}
enum Event {
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
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl Manager {
    pub fn start(settings: Settings) -> Result<Self, String> {
        let mut manager = Self {
            settings,
            monitors: BTreeMap::new(),
            ready: false,
            recovery_error: None,
            sender: None,
            receiver: None,
            worker: None,
            cleanup_pending: Default::default(),
            stopped: Default::default(),
        };
        if manager.settings.enabled {
            let journal = journal::Journal::for_user()?;
            let (sender, jobs) = mpsc::sync_channel(16);
            let (events, receiver) = mpsc::sync_channel(16);
            let stopped = manager.stopped.clone();
            let worker = std::thread::Builder::new()
                .name("amaterasu-displays".into())
                .spawn(move || {
                    worker::run(
                        journal,
                        crate::linux_control::LinuxControlClient::default(),
                        compositor::KScreen,
                        connectors::SysfsConnectors::default(),
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
                            },
                        );
                    }
                }
                Event::Created { key, result } => {
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
                    self.cleanup_pending.remove(&key);
                    match result {
                        Ok(()) => {
                            self.monitors.remove(&key);
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
            Operation::Status { identity } => (self.status(caller, identity), None),
            Operation::Remove { identity } => (self.remove(caller, &identity), None),
            Operation::Create {
                identity,
                width,
                height,
                refresh_millihz,
                name,
                dd_virtual_display_scale,
            } => {
                match ScaleSetting::try_from(dd_virtual_display_scale) {
                    Err(()) => return (failure("invalid_request"), None),
                    Ok(ScaleSetting::Retain) => {}
                    Ok(_) => return (failure("unsupported_scale"), None),
                }
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
                    return (
                        if existing.specification == specification {
                            json!({"ok":true,"monitor":existing.response()})
                        } else {
                            failure("identity_conflict")
                        },
                        None,
                    );
                }
                if self.monitors.len() >= 8 {
                    return (failure("resource_exhausted"), None);
                }
                if !self.enqueue(Work::Create {
                    owner: caller.into(),
                    specification: specification.clone(),
                }) {
                    return (failure("busy"), None);
                }
                let monitor = Monitor {
                    specification,
                    state: "creating",
                    output: None,
                    error: None,
                };
                let response = json!({"ok":true,"monitor":monitor.response()});
                self.monitors.insert(key.clone(), monitor);
                (response, Some(Admission { key }))
            }
        }
    }
    pub fn completion_lost(&mut self, admission: Option<Admission>) {
        if let Some(admission) = admission {
            let _ = self.remove(&admission.key.0, &admission.key.1);
        }
    }
    fn enqueue(&self, work: Work) -> bool {
        self.sender
            .as_ref()
            .is_some_and(|sender| sender.try_send(work).is_ok())
    }
    fn remove(&mut self, caller: &str, identity: &str) -> Value {
        if !valid_identity(identity) {
            return failure("invalid_request");
        }
        let key = (caller.to_owned(), identity.to_owned());
        if !self.ready && self.recovery_error.is_none() {
            return failure("startup_reconciliation_pending");
        }
        if !self.monitors.contains_key(&key) {
            return json!({"ok":true,"removed":true,"identity":identity});
        }
        if !self.cleanup_pending.contains(&key) {
            if !self.enqueue(Work::Remove { key: key.clone() }) {
                return failure("busy");
            }
            self.cleanup_pending.insert(key.clone());
            self.monitors.get_mut(&key).unwrap().state = "removing";
        }
        json!({"ok":true,"removed":false,"monitor":self.monitors[&key].response()})
    }
    fn status(&self, caller: &str, identity: Option<String>) -> Value {
        if identity.as_deref().is_some_and(|id| !valid_identity(id)) {
            return failure("invalid_request");
        }
        let monitors: Vec<_> = self
            .monitors
            .iter()
            .filter(|((owner, id), _)| {
                owner == caller && identity.as_ref().is_none_or(|wanted| wanted == id)
            })
            .map(|(_, monitor)| monitor.response())
            .collect();
        json!({"ok":true,"enabled":self.settings.enabled,"native_supported":true,"driver":"libvirtualdisplay",
            "ownership_authority":"unix_uid_broker","recovery_ready":self.ready,"recovery_error":self.recovery_error,"monitors":monitors,
            "activation_requested":self.settings.dd_activate_virtual_display,
            "configured_permanent_monitor_count":self.settings.dd_virtual_display_permanent_count,"permanent_monitor_runtime_supported":false})
    }
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
