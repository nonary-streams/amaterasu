//! Retained monitor mode intent and independently observed completion.
use super::*;
use crate::retained_mode::{Change, references};

impl<D: Driver> Manager<D> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare_retained_mode(
        &mut self,
        caller: &str,
        operation_id: String,
        identity: String,
        reference_id: String,
        expected_reference_ids: Vec<String>,
        expected_mode_revision: u64,
        width: u32,
        height: u32,
        refresh_millihz: u32,
        hdr_enabled: bool,
    ) -> Value {
        let Ok(expected_reference_ids) = references(expected_reference_ids) else {
            return failure("invalid_request");
        };
        let key = (caller.into(), identity);
        let Some(monitor) = self.monitors.get(&key) else {
            return failure("owned_target_unavailable");
        };
        if monitor.specification.requested_resolution_only
            && (width != monitor.specification.width || height != monitor.specification.height)
        {
            return failure("catalog_recreation_required");
        }
        let mut desired = monitor.specification.clone();
        desired.width = width;
        desired.height = height;
        desired.refresh_millihz = refresh_millihz;
        desired.hdr_enabled = hdr_enabled;
        let change = Change {
            operation_id,
            reference_id,
            expected_reference_ids,
            expected_revision: expected_mode_revision,
            old: monitor.specification.clone(),
            desired,
            completed: false,
            aborted: false,
        };
        if !change.valid(&monitor.specification) {
            return failure("invalid_request");
        }
        if !change.references_match(monitor.references.as_ref()) {
            return failure("reference_conflict");
        }
        if let Some(existing) = &monitor.retained_mode {
            if existing.operation_id == change.operation_id {
                return if existing.expected_revision == change.expected_revision
                    && existing.reference_id == change.reference_id
                    && existing.desired == change.desired
                    && existing.expected_reference_ids == change.expected_reference_ids
                {
                    existing.response()
                } else {
                    failure("reconfigure_conflict")
                };
            }
            if existing.is_pending() {
                return failure("reconfigure_pending");
            }
        }
        if expected_mode_revision
            != monitor
                .retained_mode
                .as_ref()
                .map_or(0, |change| change.mode_revision())
        {
            return failure("mode_revision_conflict");
        }
        if monitor.state != "ready" || monitor.prospective_lease.is_some() {
            return failure("owned_target_unavailable");
        }
        let observation = self.driver.inspect(
            monitor.lease,
            monitor.display_id,
            monitor.binding,
            &monitor.allocation_specification,
        );
        if !observation.is_ok_and(|observed| {
            observed.exists
                && !observed.occupied_by_other_lease
                && observed.specification_matches
                && observed.lease_display_count == 1
                && !observed.pending_departure
        }) {
            return failure("owned_target_unverified");
        }
        let monitor = self.monitors.get_mut(&key).expect("checked monitor");
        let mut record = monitor.record(caller);
        record.retained_mode = Some(change.clone());
        if self
            .journal
            .as_mut()
            .is_some_and(|journal| journal.upsert(record).is_err())
        {
            return failure("journal_unavailable");
        }
        monitor.retained_mode = Some(change.clone());
        if let Some(ready) = &mut monitor.ready {
            ready.capture_output = None;
        }
        change.response()
    }

    pub(super) fn verify_retained_mode(
        &self,
        caller: &str,
        owner: &str,
        identity: &str,
        reference_id: &str,
        operation_id: &str,
    ) -> Value {
        if !self
            .settings
            .target_verifiers
            .iter()
            .any(|verifier| verifier == caller)
        {
            return failure("permission_denied");
        }
        let Some(monitor) = self.monitors.get(&(owner.into(), identity.into())) else {
            return failure("owned_target_unavailable");
        };
        let Some(change) = &monitor.retained_mode else {
            return failure("mode_change_unavailable");
        };
        if change.aborted
            || change.operation_id != operation_id
            || change.reference_id != reference_id
            || !change.references_match(monitor.references.as_ref())
            || monitor.state != "ready"
        {
            return failure("mode_change_unavailable");
        }
        let Some(ready) = &monitor.ready else {
            return failure("owned_target_unavailable");
        };
        json!({"ok":true,"owned":true,"target_id":ready.device_path,"specification":change.desired,"active":ready.active,"mode_operation_id":change.operation_id})
    }

    fn finish_retained_mode(
        &mut self,
        caller: &str,
        identity: &str,
        reference_id: &str,
        operation_id: &str,
        abort: bool,
    ) -> Value {
        let key = (caller.into(), identity.into());
        let Some(monitor) = self.monitors.get(&key) else {
            return failure("owned_target_unavailable");
        };
        let Some(mut change) = monitor.retained_mode.clone() else {
            return failure("mode_change_unavailable");
        };
        if (change.aborted && !abort)
            || (change.completed && abort)
            || change.operation_id != operation_id
            || change.reference_id != reference_id
            || !change.references_match(monitor.references.as_ref())
            || monitor.state != "ready"
        {
            return failure("mode_change_unavailable");
        }
        let observation = self.driver.inspect(
            monitor.lease,
            monitor.display_id,
            monitor.binding,
            &monitor.allocation_specification,
        );
        if !observation.is_ok_and(|observed| {
            observed.exists
                && !observed.occupied_by_other_lease
                && observed.specification_matches
                && observed.lease_display_count == 1
                && !observed.pending_departure
        }) {
            return failure("owned_target_unverified");
        }
        let expected = if abort {
            change.old.clone()
        } else {
            change.desired.clone()
        };
        let observed = self.driver.capture_target(monitor.binding, &expected);
        let ready = match observed {
            Ok(Some(ready))
                if ready.active
                    && ready.capture_output.as_deref().is_some_and(|selector| {
                        amaterasu_module_sdk::capture::exact_target_key(selector).is_some()
                    }) =>
            {
                ready
            }
            _ => return failure("retained_mode_readback_pending"),
        };
        if monitor.ready.as_ref().is_some_and(|previous| {
            !previous
                .device_path
                .eq_ignore_ascii_case(&ready.device_path)
        }) {
            return failure("owned_target_changed");
        }
        change.completed = !abort;
        change.aborted = abort;
        let monitor = self.monitors.get_mut(&key).expect("checked monitor");
        let mut record = monitor.record(caller);
        record.specification = expected.clone();
        record.retained_mode = Some(change.clone());
        if self
            .journal
            .as_mut()
            .is_some_and(|journal| journal.upsert(record).is_err())
        {
            return failure("journal_unavailable");
        }
        monitor.specification = expected;
        monitor.retained_mode = Some(change.clone());
        monitor.ready = Some(ready);
        change.response()
    }
}

impl<D: Driver> Manager<D> {
    pub(super) fn commit_retained_mode(
        &mut self,
        caller: &str,
        identity: &str,
        reference_id: &str,
        operation_id: &str,
    ) -> Value {
        self.finish_retained_mode(caller, identity, reference_id, operation_id, false)
    }
    pub(super) fn abort_retained_mode(
        &mut self,
        caller: &str,
        identity: &str,
        reference_id: &str,
        operation_id: &str,
    ) -> Value {
        self.finish_retained_mode(caller, identity, reference_id, operation_id, true)
    }
}
