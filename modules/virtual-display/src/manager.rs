use crate::backend::{Driver, Ready};
use crate::model::{Operation, Settings, Specification, display_id, failure, valid_identity};
use crate::protocol::Binding;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const MAX_MONITORS: usize = 8;
const READINESS_TIMEOUT: Duration = Duration::from_secs(5);
const FEED_INTERVAL: Duration = Duration::from_secs(1);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

struct Monitor {
    specification: Specification,
    display_id: u64,
    lease: u64,
    binding: Option<Binding>,
    ready: Option<Ready>,
    state: &'static str,
    error: Option<&'static str>,
    created: Instant,
    next_poll: Instant,
    next_feed: Instant,
    remove_attempts: u8,
}

impl Monitor {
    fn response(&self) -> Value {
        json!({"ok":true,"monitor":{"specification":self.specification,"display_id":format!("{:016x}",self.display_id),
            "state":self.state,"ready":self.ready,"error":self.error}})
    }
    fn begin_cleanup(&mut self, error: Option<&'static str>) {
        self.ready = None;
        self.state = "removing";
        self.error = error;
        self.next_poll = Instant::now();
    }
}

/// Identifies only the native lifecycle first admitted by one request.
pub struct Admission {
    key: (String, String),
    lease: u64,
}

pub struct Manager<D: Driver> {
    settings: Settings,
    driver: D,
    monitors: BTreeMap<(String, String), Monitor>,
    cursor: usize,
}

impl<D: Driver> Manager<D> {
    pub fn new(settings: Settings, driver: D) -> Self {
        Self {
            settings,
            driver,
            monitors: BTreeMap::new(),
            cursor: 0,
        }
    }

    pub fn admit(&mut self, caller: &str, operation: Operation) -> (Value, Option<Admission>) {
        let new_key = match &operation {
            Operation::Create { identity, .. } => {
                let key = (caller.to_owned(), identity.clone());
                (!self.monitors.contains_key(&key)).then_some(key)
            }
            _ => None,
        };
        let response = self.handle(caller, operation);
        let admission = new_key.and_then(|key| {
            self.monitors.get(&key).map(|monitor| Admission {
                lease: monitor.lease,
                key,
            })
        });
        (response, admission)
    }

    pub fn completion_lost(&mut self, admission: Option<Admission>) {
        let Some(admission) = admission else { return };
        if let Some(monitor) = self.monitors.get_mut(&admission.key)
            && monitor.lease == admission.lease
        {
            monitor.begin_cleanup(Some("completion_lost"));
        }
    }

    pub fn handle(&mut self, caller: &str, operation: Operation) -> Value {
        match operation {
            Operation::Status { identity } => self.status(caller, identity),
            Operation::Create {
                identity,
                width,
                height,
                refresh_millihz,
                name,
            } => {
                if !self.settings.enabled {
                    return failure("disabled");
                }
                if !self.driver.supported() {
                    return failure("unsupported_platform");
                }
                self.create(
                    caller,
                    Specification {
                        identity,
                        width,
                        height,
                        refresh_millihz,
                        name,
                    },
                )
            }
            Operation::Remove { identity } => self.remove(caller, &identity),
        }
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
            .map(|(_, monitor)| monitor.response()["monitor"].clone())
            .collect();
        json!({"ok":true,"enabled":self.settings.enabled,"supported_platform":self.driver.supported(),
            "driver":"sunshine","monitors":monitors,"readiness":"monitor_identity_published_not_capture_ready"})
    }

    fn create(&mut self, caller: &str, specification: Specification) -> Value {
        if let Err(code) = specification.validate() {
            return failure(code);
        }
        let key = (caller.to_owned(), specification.identity.clone());
        if let Some(existing) = self.monitors.get(&key) {
            return if existing.specification == specification {
                existing.response()
            } else {
                failure("identity_conflict")
            };
        }
        if self.monitors.len() >= MAX_MONITORS {
            return failure("monitor_limit");
        }
        let display_id = display_id(caller, &specification.identity);
        if self
            .monitors
            .values()
            .any(|monitor| monitor.display_id == display_id)
        {
            return failure("identity_collision");
        }
        let mut secrets = [0u8; 40];
        if getrandom::fill(&mut secrets).is_err() {
            return failure("entropy_unavailable");
        }
        let lease = u64::from_le_bytes(secrets[..8].try_into().unwrap()) | 0x6000000000000000;
        let capability: [u8; 32] = secrets[8..].try_into().unwrap();
        let now = Instant::now();
        // Record exact ownership before issuing create: a failed acknowledgement may
        // still have created a monitor. Cleanup never searches by friendly name.
        let mut monitor = Monitor {
            specification,
            display_id,
            lease,
            binding: None,
            ready: None,
            state: "creating",
            error: None,
            created: now,
            next_poll: now,
            next_feed: now + FEED_INTERVAL,
            remove_attempts: 0,
        };
        match self
            .driver
            .create(&monitor.specification, lease, display_id, &capability)
        {
            Ok(binding) => monitor.binding = Some(binding),
            Err(code) => monitor.begin_cleanup(Some(code)),
        }
        let reply = monitor.response();
        self.monitors.insert(key, monitor);
        reply
    }

    fn remove(&mut self, caller: &str, identity: &str) -> Value {
        if !valid_identity(identity) {
            return failure("invalid_request");
        }
        let Some(monitor) = self.monitors.get_mut(&(caller.into(), identity.into())) else {
            return json!({"ok":true,"removed":true,"identity":identity});
        };
        monitor.begin_cleanup(None);
        monitor.response()
    }

    pub fn tick(&mut self) {
        if self.monitors.is_empty() {
            return;
        }
        self.cursor %= self.monitors.len();
        let key = self
            .monitors
            .keys()
            .nth(self.cursor)
            .cloned()
            .expect("bounded existing cursor");
        self.cursor += 1;
        let monitor = self
            .monitors
            .get_mut(&key)
            .expect("selected monitor exists");
        let now = Instant::now();
        if now < monitor.next_poll {
            return;
        }
        monitor.next_poll = now + POLL_INTERVAL;
        let removed = if matches!(monitor.state, "removing" | "cleanup_failed") {
            cleanup(&mut self.driver, monitor)
        } else {
            maintain(&mut self.driver, monitor, now);
            false
        };
        if removed {
            self.monitors.remove(&key);
        }
    }
}

impl<D: Driver> Drop for Manager<D> {
    fn drop(&mut self) {
        // Best effort exact owned removals only. If process teardown interrupts this,
        // stopped lease feeding bounds orphan lifetime in the driver (ten seconds).
        for monitor in self.monitors.values() {
            let _ = self.driver.remove(monitor.lease, monitor.display_id);
        }
    }
}

fn maintain(driver: &mut impl Driver, monitor: &mut Monitor, now: Instant) {
    if now >= monitor.next_feed {
        if let Err(code) = driver.feed(monitor.lease) {
            monitor.begin_cleanup(Some(code));
            return;
        }
        monitor.next_feed = now + FEED_INTERVAL;
    }
    match driver.inspect(monitor.lease, monitor.display_id, monitor.binding) {
        Ok(observation) if observation.exists && observation.ready.is_some() => {
            monitor.ready = observation.ready;
            monitor.state = "ready";
        }
        Ok(observation) if !observation.exists => monitor.begin_cleanup(Some("monitor_departed")),
        Ok(_) if now.duration_since(monitor.created) >= READINESS_TIMEOUT => {
            monitor.begin_cleanup(Some("readiness_timeout"))
        }
        Ok(_) => {}
        Err(code) => monitor.begin_cleanup(Some(code)),
    }
}

fn cleanup(driver: &mut impl Driver, monitor: &mut Monitor) -> bool {
    if monitor.remove_attempts < 3 {
        monitor.remove_attempts += 1;
        if let Err(code) = driver.remove(monitor.lease, monitor.display_id) {
            monitor.error = Some(code);
        }
    }
    match driver.inspect(monitor.lease, monitor.display_id, monitor.binding) {
        Ok(observation) if !observation.exists && !observation.pending_departure => true,
        Ok(_) => {
            if monitor.remove_attempts >= 3 {
                monitor.state = "cleanup_failed";
            }
            false
        }
        Err(code) => {
            monitor.state = "cleanup_failed";
            monitor.error = Some(code);
            false
        }
    }
}

#[cfg(test)]
mod tests;
