use crate::backend::{Driver, Ready};
use crate::journal::{Journal, Record};
use crate::model::{Operation, Settings, Specification, display_id, failure, valid_identity};
use crate::protocol::Binding;
use crate::scale::ScaleSetting;
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
    prospective_lease: Option<u64>,
    capability: [u8; 32],
    binding: Option<Binding>,
    ready: Option<Ready>,
    state: &'static str,
    error: Option<&'static str>,
    readiness_started: Instant,
    next_poll: Instant,
    next_feed: Instant,
    remove_attempts: u8,
}

impl Monitor {
    fn record(&self, owner: &str) -> Record {
        Record {
            owner: owner.into(),
            specification: self.specification.clone(),
            display_id: self.display_id,
            lease: self.lease,
            prospective_lease: self.prospective_lease,
            capability: self.capability,
            binding: self.binding,
        }
    }
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
    journal: Option<Journal>,
    cursor: usize,
}

impl<D: Driver> Manager<D> {
    pub fn new(settings: Settings, driver: D) -> Self {
        Self {
            settings,
            driver,
            monitors: BTreeMap::new(),
            journal: None,
            cursor: 0,
        }
    }

    pub fn with_journal(settings: Settings, driver: D, journal: Journal) -> Result<Self, String> {
        let mut manager = Self::new(settings, driver);
        let records: Vec<_> = journal.records().cloned().collect();
        manager.journal = Some(journal);
        for record in records {
            manager.recover(record)?;
        }
        Ok(manager)
    }

    fn recover(&mut self, record: Record) -> Result<(), String> {
        let now = Instant::now();
        let key = (record.owner.clone(), record.specification.identity.clone());
        let mut monitor = Monitor {
            specification: record.specification,
            display_id: record.display_id,
            lease: record.lease,
            prospective_lease: record.prospective_lease,
            capability: record.capability,
            binding: record.binding,
            ready: None,
            state: "recovered",
            error: None,
            readiness_started: now,
            next_poll: now,
            next_feed: now,
            remove_attempts: 0,
        };
        monitor.state = "reclaim_pending";
        self.monitors.insert(key, monitor);
        self.reconcile_recovery()
    }

    fn reconcile_recovery(&mut self) -> Result<(), String> {
        let keys: Vec<_> = self
            .monitors
            .iter()
            .filter(|(_, monitor)| monitor.state == "reclaim_pending")
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            let monitor = self.monitors.get_mut(&key).expect("recovery key");
            if Instant::now() < monitor.next_poll {
                continue;
            }
            monitor.next_poll = Instant::now() + POLL_INTERVAL;
            let old = self.driver.inspect(
                monitor.lease,
                monitor.display_id,
                monitor.binding,
                &monitor.specification,
            );
            let next = monitor.prospective_lease.map(|lease| {
                self.driver.inspect(
                    lease,
                    monitor.display_id,
                    monitor.binding,
                    &monitor.specification,
                )
            });
            let (Ok(old), next) = (old, next) else {
                continue;
            };
            let next = match next {
                Some(Ok(next)) => Some(next),
                Some(Err(_)) => continue,
                None => None,
            };
            if (!old.exists
                && next.as_ref().is_none_or(|next| !next.exists)
                && (old.occupied_by_other_lease
                    || next
                        .as_ref()
                        .is_some_and(|next| next.occupied_by_other_lease)))
                || (old.exists && (!old.specification_matches || old.lease_display_count != 1))
                || next.as_ref().is_some_and(|next| {
                    next.exists && (!next.specification_matches || next.lease_display_count != 1)
                })
            {
                monitor.state = "ownership_conflict";
                monitor.error = Some("identity_conflict");
                continue;
            }
            if next
                .as_ref()
                .is_some_and(|next| next.exists && !next.pending_departure)
                && !old.exists
            {
                let mut record = monitor.record(&key.0);
                record.lease = record.prospective_lease.take().expect("prospective lease");
                self.journal
                    .as_mut()
                    .expect("journal")
                    .upsert(record.clone())?;
                monitor.lease = record.lease;
                monitor.prospective_lease = None;
                monitor.state = "recovered";
                continue;
            }
            if !old.exists
                && !old.pending_departure
                && next
                    .as_ref()
                    .is_none_or(|next| !next.exists && !next.pending_departure)
            {
                self.journal
                    .as_mut()
                    .expect("journal")
                    .delete(&key.0, &key.1)?;
                self.monitors.remove(&key);
                continue;
            }
            if !old.exists
                || old.pending_departure
                || next
                    .as_ref()
                    .is_some_and(|next| next.exists || next.pending_departure)
            {
                continue;
            }
            if monitor.prospective_lease.is_none() {
                let new_lease = random_lease().ok_or("entropy_unavailable")?;
                monitor.prospective_lease = Some(new_lease);
                self.journal
                    .as_mut()
                    .expect("journal")
                    .upsert(monitor.record(&key.0))?;
            }
            let new_lease = monitor.prospective_lease.expect("prepared lease");
            if self
                .driver
                .reclaim(monitor.display_id, new_lease, &monitor.capability)
                .is_ok()
            {
                let Ok(observation) = self.driver.inspect(
                    new_lease,
                    monitor.display_id,
                    monitor.binding,
                    &monitor.specification,
                ) else {
                    continue;
                };
                if !observation.exists
                    || !observation.specification_matches
                    || observation.lease_display_count != 1
                    || observation.pending_departure
                {
                    continue;
                }
                let mut record = monitor.record(&key.0);
                record.lease = new_lease;
                record.prospective_lease = None;
                self.journal.as_mut().expect("journal").upsert(record)?;
                monitor.lease = new_lease;
                monitor.prospective_lease = None;
                monitor.state = "recovered";
            }
        }
        Ok(())
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
                dd_virtual_display_scale,
            } => {
                let scale = match dd_virtual_display_scale.map(ScaleSetting::try_from) {
                    None => self.settings.dd_virtual_display_scale,
                    Some(Ok(scale)) => scale,
                    Some(Err(())) => return failure("invalid_request"),
                };
                if scale != ScaleSetting::Retain {
                    return failure("unsupported_scale");
                }
                if !self.settings.enabled {
                    return failure("disabled");
                }
                if !self.settings.dd_use_sunshine_virtual_display_driver {
                    return failure("unsupported_driver");
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
        let driver = if self.settings.dd_use_sunshine_virtual_display_driver {
            "sunshine"
        } else {
            "unsupported"
        };
        let activation_state = if self.settings.dd_activate_virtual_display {
            "unsupported"
        } else {
            "not_requested"
        };
        let activation_error = self
            .settings
            .dd_activate_virtual_display
            .then_some("unsupported_activation");
        json!({"ok":true,
            "enabled":self.settings.enabled,
            "dd_activate_virtual_display":self.settings.dd_activate_virtual_display,
            "dd_use_sunshine_virtual_display_driver":self.settings.dd_use_sunshine_virtual_display_driver,
            "dd_virtual_display_permanent_count":self.settings.dd_virtual_display_permanent_count.unwrap_or_default(),
            "dd_virtual_display_permanent_count_configured":self.settings.dd_virtual_display_permanent_count.is_some(),
            "permanent_monitor_runtime_supported":false,
            "permanent_monitor_runtime_state":"unsupported",
            "supported_platform":self.driver.supported(),
            "driver":driver,
            "activation_supported":false,
            "activation_state":activation_state,
            "activation_error":activation_error,
            "monitors":monitors,"readiness":"monitor_identity_published_not_capture_ready"})
    }

    fn create(&mut self, caller: &str, specification: Specification) -> Value {
        if let Err(code) = specification.validate() {
            return failure(code);
        }
        let key = (caller.to_owned(), specification.identity.clone());
        if let Some(existing) = self.monitors.get(&key) {
            if matches!(existing.state, "ownership_conflict" | "reclaim_pending") {
                return failure(if existing.state == "ownership_conflict" {
                    "ownership_conflict"
                } else {
                    "recovery_pending"
                });
            }
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
        if self
            .monitors
            .values()
            .any(|monitor| monitor.lease == lease || monitor.prospective_lease == Some(lease))
        {
            return failure("lease_collision");
        }
        let capability: [u8; 32] = secrets[8..].try_into().unwrap();
        let now = Instant::now();
        // Record exact ownership before issuing create: a failed acknowledgement may
        // still have created a monitor. Cleanup never searches by friendly name.
        let mut monitor = Monitor {
            specification,
            display_id,
            lease,
            prospective_lease: None,
            capability,
            binding: None,
            ready: None,
            state: "creating",
            error: None,
            readiness_started: now,
            next_poll: now,
            next_feed: now + FEED_INTERVAL,
            remove_attempts: 0,
        };
        if let Some(journal) = &mut self.journal
            && journal.upsert(monitor.record(&key.0)).is_err()
        {
            return failure("journal_unavailable");
        }
        match self
            .driver
            .create(&monitor.specification, lease, display_id, &capability)
        {
            Ok(binding) => {
                monitor.binding = Some(binding);
                if let Some(journal) = &mut self.journal
                    && journal.upsert(monitor.record(&key.0)).is_err()
                {
                    monitor.begin_cleanup(Some("journal_unavailable"));
                }
            }
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
        if matches!(monitor.state, "ownership_conflict" | "reclaim_pending") {
            return failure(if monitor.state == "ownership_conflict" {
                "ownership_conflict"
            } else {
                "recovery_pending"
            });
        }
        if monitor.state == "cleanup_failed" {
            // Only an explicit request by the owning caller opens another
            // bounded removal window for this exact lease and display.
            monitor.remove_attempts = 0;
        }
        monitor.begin_cleanup(None);
        monitor.response()
    }

    pub fn tick(&mut self) {
        if self.reconcile_recovery().is_err() {
            return;
        }
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
        if matches!(monitor.state, "reclaim_pending" | "ownership_conflict") {
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
            if self
                .journal
                .as_mut()
                .is_none_or(|journal| journal.delete(&key.0, &key.1).is_ok())
            {
                self.monitors.remove(&key);
            } else if let Some(monitor) = self.monitors.get_mut(&key) {
                monitor.state = "cleanup_failed";
                monitor.error = Some("journal_unavailable");
            }
        }
    }
}

impl<D: Driver> Drop for Manager<D> {
    fn drop(&mut self) {
        // Best effort exact owned removals only. If process teardown interrupts this,
        // stopped lease feeding bounds orphan lifetime in the driver (ten seconds).
        for monitor in self.monitors.values() {
            if monitor.prospective_lease.is_none()
                && !matches!(monitor.state, "ownership_conflict" | "reclaim_pending")
            {
                let _ = self.driver.remove(monitor.lease, monitor.display_id);
            }
        }
    }
}

fn maintain(driver: &mut impl Driver, monitor: &mut Monitor, now: Instant) {
    match driver.inspect(
        monitor.lease,
        monitor.display_id,
        monitor.binding,
        &monitor.specification,
    ) {
        Ok(observation)
            if observation.occupied_by_other_lease
                || (observation.exists
                    && (!observation.specification_matches
                        || observation.lease_display_count != 1)) =>
        {
            monitor.ready = None;
            monitor.state = "ownership_conflict";
            monitor.error = Some("identity_conflict");
        }
        Ok(observation) if observation.pending_departure => {
            monitor.begin_cleanup(Some("lease_departing"));
        }
        Ok(observation) if observation.exists && observation.ready.is_some() => {
            monitor.ready = observation.ready;
            monitor.state = "ready";
        }
        Ok(observation) if !observation.exists => monitor.begin_cleanup(Some("monitor_departed")),
        Ok(_) => {
            if monitor.binding.is_none() {
                monitor.state = "recovered";
            } else if monitor.ready.take().is_some() {
                // CCD can withdraw a published target while the driver lease still
                // exists. Stop advertising the stale path and allow it to settle.
                monitor.state = "creating";
                monitor.readiness_started = now;
            } else if now.duration_since(monitor.readiness_started) >= READINESS_TIMEOUT {
                monitor.begin_cleanup(Some("readiness_timeout"));
            }
        }
        Err(code) => monitor.begin_cleanup(Some(code)),
    }
    if matches!(monitor.state, "removing" | "ownership_conflict") {
        return;
    }
    if now >= monitor.next_feed {
        if let Err(code) = driver.feed(monitor.lease) {
            monitor.begin_cleanup(Some(code));
            return;
        }
        monitor.next_feed = now + FEED_INTERVAL;
    }
}

fn cleanup(driver: &mut impl Driver, monitor: &mut Monitor) -> bool {
    match driver.inspect(
        monitor.lease,
        monitor.display_id,
        monitor.binding,
        &monitor.specification,
    ) {
        Ok(observation)
            if observation.occupied_by_other_lease
                || (observation.exists
                    && (!observation.specification_matches
                        || observation.lease_display_count != 1)) =>
        {
            monitor.state = "ownership_conflict";
            monitor.error = Some("identity_conflict");
            return false;
        }
        Err(code) => {
            monitor.state = "cleanup_failed";
            monitor.error = Some(code);
            return false;
        }
        _ => {}
    }
    if monitor.remove_attempts < 3 {
        monitor.remove_attempts += 1;
        if let Err(code) = driver.remove(monitor.lease, monitor.display_id) {
            monitor.error = Some(code);
        }
    }
    match driver.inspect(
        monitor.lease,
        monitor.display_id,
        monitor.binding,
        &monitor.specification,
    ) {
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

fn random_lease() -> Option<u64> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).ok()?;
    Some(u64::from_le_bytes(bytes) | 0x6000000000000000)
}

#[cfg(test)]
mod tests;
