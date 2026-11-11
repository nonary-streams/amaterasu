use crate::backend::{Driver, Ready};
use crate::journal::{Journal, Record};
use crate::model::{
    Operation, Settings, Specification, display_id, failure, valid_identity, valid_reference_id,
};
use crate::protocol::Binding;
use crate::scale::ScaleSetting;
mod permanent;
mod retained_mode;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

const MAX_MONITORS: usize = 8;
const READINESS_TIMEOUT: Duration = Duration::from_secs(5);
const FEED_INTERVAL: Duration = Duration::from_secs(1);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

struct Monitor {
    specification: Specification,
    allocation_specification: Specification,
    retained_mode: Option<crate::retained_mode::Change>,
    display_id: u64,
    lease: u64,
    prospective_lease: Option<u64>,
    capability: [u8; 32],
    references: Option<BTreeSet<String>>,
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
            allocation_specification: Some(self.allocation_specification.clone()),
            retained_mode: self.retained_mode.clone(),
            display_id: self.display_id,
            lease: self.lease,
            prospective_lease: self.prospective_lease,
            capability: self.capability,
            binding: self.binding,
            references: self.references.clone(),
        }
    }
    fn response(&self, reference_id: Option<&str>) -> Value {
        let mode_revision = self
            .retained_mode
            .as_ref()
            .map_or(0, |change| change.mode_revision());
        let retained_mode = self.retained_mode.as_ref().map(|change| change.response());
        let capture_output = self
            .ready
            .as_ref()
            .filter(|ready| {
                ready.active
                    && self
                        .retained_mode
                        .as_ref()
                        .is_none_or(|change| !change.is_pending())
            })
            .and_then(|ready| ready.capture_output.as_deref())
            .filter(|output| amaterasu_module_sdk::capture::exact_target_key(output).is_some());
        let reference_owned = reference_id.is_some_and(|reference| {
            self.references
                .as_ref()
                .is_some_and(|references| references.contains(reference))
        });
        json!({
            "ok": true,
            "monitor": {
                "specification": self.specification,
                "mode_revision": mode_revision,
                "retained_mode": retained_mode,
                "display_id": format!("{:016x}", self.display_id),
                "state": self.state,
                "ready": self.ready,
                "error": self.error,
                "capture_output": capture_output,
            },
            "reference_owned": reference_owned,
        })
    }
    fn finish_recovery(&mut self) {
        if self.references.as_ref().is_some_and(BTreeSet::is_empty) {
            self.begin_cleanup(None);
        } else {
            self.state = "recovered";
        }
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
    reference_id: Option<String>,
}

pub struct Manager<D: Driver> {
    settings: Settings,
    driver: D,
    monitors: BTreeMap<(String, String), Monitor>,
    journal: Option<Journal>,
    cursor: usize,
    permanent: permanent::RuntimeState,
}

impl<D: Driver> Manager<D> {
    pub fn new(settings: Settings, mut driver: D) -> Self {
        // Startup may perform bounded device I/O. Tick and public requests never
        // retry this global configuration effect.
        let permanent = permanent::synchronize(&settings, &mut driver);
        Self {
            settings,
            driver,
            monitors: BTreeMap::new(),
            journal: None,
            cursor: 0,
            permanent,
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
            allocation_specification: record
                .allocation_specification
                .unwrap_or_else(|| record.specification.clone()),
            retained_mode: record.retained_mode,
            specification: record.specification,
            display_id: record.display_id,
            lease: record.lease,
            prospective_lease: record.prospective_lease,
            capability: record.capability,
            references: record.references,
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
                &monitor.allocation_specification,
            );
            let next = monitor.prospective_lease.map(|lease| {
                self.driver.inspect(
                    lease,
                    monitor.display_id,
                    monitor.binding,
                    &monitor.allocation_specification,
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
                monitor.finish_recovery();
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
                if new_lease == monitor.lease {
                    return Err("lease_collision".into());
                }
                let mut record = monitor.record(&key.0);
                record.prospective_lease = Some(new_lease);
                self.journal.as_mut().expect("journal").upsert(record)?;
                // Only expose the new lease to the reclaim path after its
                // ownership intent has been durably recorded.
                monitor.prospective_lease = Some(new_lease);
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
                    &monitor.allocation_specification,
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
                monitor.finish_recovery();
            }
        }
        Ok(())
    }

    pub fn admit(&mut self, caller: &str, operation: Operation) -> (Value, Option<Admission>) {
        let (key, needs_admission, reference_id) = match &operation {
            Operation::Create {
                identity,
                reference_id,
                ..
            } => {
                let key = (caller.to_owned(), identity.clone());
                let needs_admission = self.monitors.get(&key).is_none_or(|monitor| {
                    reference_id.as_ref().is_some_and(|reference| {
                        !monitor
                            .references
                            .as_ref()
                            .is_some_and(|references| references.contains(reference))
                    })
                });
                (Some(key), needs_admission, reference_id.clone())
            }
            _ => (None, false, None),
        };
        let response = self.handle(caller, operation);
        let admission = if response["ok"] == true && needs_admission {
            key.and_then(|key| {
                self.monitors.get(&key).map(|monitor| Admission {
                    lease: monitor.lease,
                    key,
                    reference_id,
                })
            })
        } else {
            None
        };
        (response, admission)
    }

    pub fn completion_lost(&mut self, admission: Option<Admission>) {
        let Some(admission) = admission else { return };
        if let Some(monitor) = self.monitors.get_mut(&admission.key)
            && monitor.lease == admission.lease
        {
            if let Some(reference_id) = admission.reference_id {
                if let Some(references) = &mut monitor.references {
                    let released = references.remove(&reference_id);
                    let no_references = references.is_empty();
                    if let Some(journal) = &mut self.journal
                        && journal.upsert(monitor.record(&admission.key.0)).is_err()
                    {
                        if released {
                            monitor
                                .references
                                .as_mut()
                                .expect("reference monitor")
                                .insert(reference_id);
                        }
                        monitor.error = Some("journal_unavailable");
                        return;
                    }
                    if no_references {
                        monitor.begin_cleanup(Some("completion_lost"));
                    }
                }
            } else {
                monitor.begin_cleanup(Some("completion_lost"));
            }
        }
    }

    pub fn handle(&mut self, caller: &str, operation: Operation) -> Value {
        match operation {
            Operation::PrepareRetainedMode {
                operation_id,
                identity,
                reference_id,
                expected_reference_ids,
                expected_mode_revision,
                width,
                height,
                refresh_millihz,
                hdr_enabled,
            } => self.prepare_retained_mode(
                caller,
                operation_id,
                identity,
                reference_id,
                expected_reference_ids,
                expected_mode_revision,
                width,
                height,
                refresh_millihz,
                hdr_enabled,
            ),
            Operation::VerifyRetainedMode {
                owner,
                identity,
                reference_id,
                operation_id,
            } => self.verify_retained_mode(caller, &owner, &identity, &reference_id, &operation_id),
            Operation::CommitRetainedMode {
                operation_id,
                identity,
                reference_id,
            } => self.commit_retained_mode(caller, &identity, &reference_id, &operation_id),
            Operation::AbortRetainedMode {
                operation_id,
                identity,
                reference_id,
            } => self.abort_retained_mode(caller, &identity, &reference_id, &operation_id),
            Operation::VerifyOwnerTarget {
                owner,
                identity,
                reference_id,
            } => self.verify_owner_target(caller, &owner, &identity, &reference_id),
            Operation::ReconfigureModeOnlyStatus {
                operation_id,
                identity,
                reference_id,
                borrow_id,
                expected_topology_domain_id,
                expected_topology_revision,
            } => {
                drop((
                    operation_id,
                    identity,
                    reference_id,
                    borrow_id,
                    expected_topology_domain_id,
                    expected_topology_revision,
                ));
                failure("unsupported_binding")
            }
            Operation::ReconfigureModeOnly {
                operation_id,
                identity,
                reference_id,
                borrow_id,
                width,
                height,
                refresh_millihz,
                expected_topology_domain_id,
                expected_topology_revision,
            } => {
                drop((
                    operation_id,
                    identity,
                    reference_id,
                    borrow_id,
                    width,
                    height,
                    refresh_millihz,
                    expected_topology_domain_id,
                    expected_topology_revision,
                ));
                failure("unsupported_binding")
            }
            Operation::ReconfigureStatus {
                operation_id,
                identity,
                reference_id,
                borrow_id,
                expected_topology_domain_id,
                expected_topology_revision,
            } => {
                drop((
                    operation_id,
                    identity,
                    reference_id,
                    borrow_id,
                    expected_topology_domain_id,
                    expected_topology_revision,
                ));
                failure("unsupported_binding")
            }
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
            } => {
                drop((
                    operation_id,
                    identity,
                    reference_id,
                    borrow_id,
                    width,
                    height,
                    refresh_millihz,
                    expected_topology_domain_id,
                    expected_topology_revision,
                ));
                failure("unsupported_binding")
            }
            Operation::Borrow {
                identity,
                reference_id,
            } => {
                drop((identity, reference_id));
                failure("unsupported_binding")
            }
            Operation::ValidateBorrow {
                identity,
                reference_id,
                borrow_id,
            }
            | Operation::ReleaseBorrow {
                identity,
                reference_id,
                borrow_id,
            } => {
                drop((identity, reference_id, borrow_id));
                failure("unsupported_binding")
            }
            Operation::Status {
                identity,
                reference_id,
            } => self.status(caller, identity, reference_id),
            Operation::Create {
                hdr_enabled,
                requested_resolution_only,
                identity,
                width,
                height,
                refresh_millihz,
                name,
                reference_id,
                dd_virtual_display_scale,
                virtual_display_layout,
                expected_topology_revision,
                expected_topology_domain_id,
            } => {
                // Extended creation preserves peer outputs. Activation remains
                // the display coordinator's job; only independent readback can
                // publish the optional exact capture selector.
                if virtual_display_layout
                    .is_some_and(|layout| layout != crate::model::VirtualDisplayLayout::Extended)
                    || expected_topology_revision.is_some()
                    || expected_topology_domain_id.is_some()
                {
                    return failure("unsupported_topology_domain");
                }
                if reference_id
                    .as_deref()
                    .is_some_and(|reference| !valid_reference_id(reference))
                {
                    return failure("invalid_request");
                }
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
                        hdr_enabled,
                        requested_resolution_only,
                        identity,
                        width,
                        height,
                        refresh_millihz,
                        name,
                    },
                    reference_id,
                )
            }
            Operation::Remove {
                identity,
                reference_id,
            } => {
                if reference_id
                    .as_deref()
                    .is_some_and(|reference| !valid_reference_id(reference))
                {
                    return failure("invalid_request");
                }
                self.remove(caller, &identity, reference_id.as_deref())
            }
        }
    }

    fn verify_owner_target(
        &self,
        caller: &str,
        owner: &str,
        identity: &str,
        reference_id: &str,
    ) -> Value {
        if !self
            .settings
            .target_verifiers
            .iter()
            .any(|verifier| verifier == caller)
        {
            return failure("permission_denied");
        }
        if !crate::model::valid_principal(owner)
            || !valid_identity(identity)
            || !valid_reference_id(reference_id)
        {
            return failure("invalid_request");
        }
        let Some(monitor) = self.monitors.get(&(owner.into(), identity.into())) else {
            return failure("owned_target_unavailable");
        };
        if monitor
            .retained_mode
            .as_ref()
            .is_some_and(|change| change.is_pending())
        {
            return failure("reconfigure_pending");
        }
        if !monitor
            .references
            .as_ref()
            .is_some_and(|references| references.contains(reference_id))
            || monitor.state != "ready"
        {
            return failure("owned_target_unavailable");
        }
        let Some(ready) = &monitor.ready else {
            return failure("owned_target_unavailable");
        };
        json!({"ok":true,"owned":true,"target_id":ready.device_path,"specification":monitor.specification,"active":ready.active})
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
            .map(|(_, monitor)| monitor.response(reference_id.as_deref())["monitor"].clone())
            .collect();
        let reference_owned = match (&identity, &reference_id) {
            (Some(identity), Some(reference_id)) => self
                .monitors
                .get(&(caller.to_owned(), identity.clone()))
                .and_then(|monitor| monitor.references.as_ref())
                .is_some_and(|references| references.contains(reference_id)),
            _ => false,
        };
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
            "permanent_monitor_runtime_supported":self.permanent.supported,
            "permanent_monitor_runtime_state":self.permanent.state,
            "permanent_monitor_runtime_error":self.permanent.error,
            "permanent_monitor_runtime_count":self.permanent.observed_count,
            "supported_platform":self.driver.supported(),
            "requested_resolution_catalog_supported":self.driver.requested_resolution_catalog_supported(),
            "driver":driver,
            "activation_supported":false,
            "activation_state":activation_state,
            "activation_error":activation_error,
            "monitors":monitors,"reference_owned":reference_id.map(|_| reference_owned),
            "readiness":"monitor_identity_with_optional_exact_capture_binding"})
    }

    fn create(
        &mut self,
        caller: &str,
        specification: Specification,
        reference_id: Option<String>,
    ) -> Value {
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
            if existing.specification != specification {
                return failure("identity_conflict");
            }
            let Some(reference_id) = reference_id else {
                return if existing.references.is_none() {
                    existing.response(None)
                } else {
                    failure("reference_conflict")
                };
            };
            let Some(references) = existing.references.as_ref() else {
                return failure("reference_conflict");
            };
            if references.contains(&reference_id) {
                return existing.response(Some(&reference_id));
            }
            if existing
                .retained_mode
                .as_ref()
                .is_some_and(|change| change.is_pending())
            {
                return failure("reconfigure_pending");
            }
            if existing.state == "removing" || existing.state == "cleanup_failed" {
                return failure("cleanup_pending");
            }
            if references.len() >= 64 {
                return failure("reference_limit");
            }
            let monitor = self.monitors.get_mut(&key).expect("existing monitor");
            monitor
                .references
                .as_mut()
                .expect("reference monitor")
                .insert(reference_id.clone());
            if let Some(journal) = &mut self.journal
                && journal.upsert(monitor.record(&key.0)).is_err()
            {
                monitor
                    .references
                    .as_mut()
                    .expect("reference monitor")
                    .remove(&reference_id);
                return failure("journal_unavailable");
            }
            return monitor.response(Some(&reference_id));
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
        if let Err(error) = self.driver.validate_create(&specification) {
            return failure(error);
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
            allocation_specification: specification.clone(),
            retained_mode: None,
            specification,
            display_id,
            lease,
            prospective_lease: None,
            capability,
            references: reference_id
                .as_ref()
                .map(|reference_id| BTreeSet::from([reference_id.clone()])),
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
        match self.driver.create(
            &monitor.allocation_specification,
            lease,
            display_id,
            &capability,
        ) {
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
        let reply = monitor.response(reference_id.as_deref());
        self.monitors.insert(key, monitor);
        reply
    }

    fn remove(&mut self, caller: &str, identity: &str, reference_id: Option<&str>) -> Value {
        if !valid_identity(identity) {
            return failure("invalid_request");
        }
        let key = (caller.to_owned(), identity.to_owned());
        let Some(monitor) = self.monitors.get_mut(&key) else {
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
        if let Some(reference_id) = reference_id {
            if monitor.references.is_none() {
                return failure("reference_conflict");
            }
            let (released, no_references) = {
                let references = monitor
                    .references
                    .as_mut()
                    .expect("reference monitor checked above");
                let released = references.remove(reference_id);
                (released, references.is_empty())
            };
            if let Some(journal) = &mut self.journal
                && journal.upsert(monitor.record(&key.0)).is_err()
            {
                if released {
                    monitor
                        .references
                        .as_mut()
                        .expect("reference monitor")
                        .insert(reference_id.into());
                }
                return failure("journal_unavailable");
            }
            if !no_references {
                let still_owned = monitor
                    .references
                    .as_ref()
                    .expect("reference monitor")
                    .contains(reference_id);
                return json!({"ok":true,"removed":false,"identity":identity,
                    "reference_released":released,"reference_owned":still_owned});
            }
        } else if let Some(references) = &mut monitor.references {
            let previous = references.clone();
            references.clear();
            if let Some(journal) = &mut self.journal
                && journal.upsert(monitor.record(&key.0)).is_err()
            {
                monitor.references = Some(previous);
                return failure("journal_unavailable");
            }
        }
        monitor.begin_cleanup(None);
        monitor.response(reference_id)
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
        &monitor.allocation_specification,
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
            let mut ready = observation.ready;
            if monitor.specification != monitor.allocation_specification {
                match driver.capture_target(monitor.binding, &monitor.specification) {
                    Ok(Some(observed)) => ready = Some(observed),
                    _ => {
                        if let Some(ready) = &mut ready {
                            ready.capture_output = None;
                            ready.capture_error = Some("effective_mode_unverified");
                        }
                    }
                }
            }
            if monitor
                .retained_mode
                .as_ref()
                .is_some_and(|change| change.is_pending())
                && let Some(ready) = &mut ready
            {
                ready.capture_output = None;
            }
            monitor.ready = ready;
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
        &monitor.allocation_specification,
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
        &monitor.allocation_specification,
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
