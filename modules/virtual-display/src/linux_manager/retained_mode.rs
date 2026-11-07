//! The actor journals retained-mode admission and verifies completion natively.
use super::*;
use crate::retained_mode::{Change, references};

impl Manager {
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
        if self.borrows.values().any(|borrow| borrow.key == key) {
            return failure("topology_borrowed");
        }
        let Some(monitor) = self.monitors.get(&key) else {
            return failure("owned_target_unavailable");
        };
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
        if monitor.state != "ready" || !self.ready || self.pending_mutations != 0 {
            return failure("owned_target_unavailable");
        }
        if let Some(existing) = &monitor.retained_mode {
            if existing.operation_id == change.operation_id {
                if existing.expected_revision != change.expected_revision
                    || existing.reference_id != change.reference_id
                    || existing.desired != change.desired
                    || existing.expected_reference_ids != change.expected_reference_ids
                {
                    return failure("reconfigure_conflict");
                }
                if monitor.retained_mode_pending {
                    return json!({"ok":true,"state":"pending","operation_id":change.operation_id});
                }
                if monitor.retained_mode_durable {
                    return existing.response();
                }
            } else if existing.is_pending() {
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
        if !self.enqueue(Work::RetainedMode {
            key: key.clone(),
            change: change.clone(),
            step: crate::retained_mode::Step::Prepare,
        }) {
            return failure("busy");
        }
        let monitor = self.monitors.get_mut(&key).expect("checked monitor");
        monitor.retained_mode = Some(change.clone());
        monitor.retained_mode_pending = true;
        monitor.retained_mode_durable = false;
        json!({"ok":true,"state":"pending","operation_id":change.operation_id})
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
        let key = (owner.into(), identity.into());
        let Some(monitor) = self.monitors.get(&key) else {
            return failure("owned_target_unavailable");
        };
        let Some(change) = &monitor.retained_mode else {
            return failure("mode_change_unavailable");
        };
        if change.aborted
            || change.operation_id != operation_id
            || change.reference_id != reference_id
            || !change.references_match(monitor.references.as_ref())
            || !monitor.retained_mode_durable
            || monitor.retained_mode_pending
            || monitor.state != "ready"
            || self.borrows.values().any(|borrow| borrow.key == key)
        {
            return failure("mode_change_unavailable");
        }
        let Some(output) = &monitor.output else {
            return failure("owned_target_unavailable");
        };
        json!({"ok":true,"owned":true,"target_id":output,"specification":change.desired,"active":true,"mode_operation_id":change.operation_id})
    }

    pub(super) fn commit_retained_mode(
        &mut self,
        caller: &str,
        identity: &str,
        reference_id: &str,
        operation_id: &str,
    ) -> Value {
        let key = (caller.into(), identity.into());
        let Some(monitor) = self.monitors.get(&key) else {
            return failure("owned_target_unavailable");
        };
        let Some(change) = monitor.retained_mode.clone() else {
            return failure("mode_change_unavailable");
        };
        if change.aborted
            || change.operation_id != operation_id
            || change.reference_id != reference_id
            || !change.references_match(monitor.references.as_ref())
            || !monitor.retained_mode_durable
            || monitor.state != "ready"
            || self.borrows.values().any(|borrow| borrow.key == key)
        {
            return failure("mode_change_unavailable");
        }
        if monitor.retained_mode_pending {
            return json!({"ok":true,"state":"pending","operation_id":operation_id});
        }
        if change.completed {
            return change.response();
        }
        if !self.enqueue(Work::RetainedMode {
            key: key.clone(),
            change,
            step: crate::retained_mode::Step::Commit,
        }) {
            return failure("busy");
        }
        self.monitors
            .get_mut(&key)
            .expect("checked monitor")
            .retained_mode_pending = true;
        json!({"ok":true,"state":"pending","operation_id":operation_id})
    }
}

impl Manager {
    pub(super) fn abort_retained_mode(
        &mut self,
        caller: &str,
        identity: &str,
        reference_id: &str,
        operation_id: &str,
    ) -> Value {
        let key = (caller.into(), identity.into());
        let Some(monitor) = self.monitors.get(&key) else {
            return failure("owned_target_unavailable");
        };
        let Some(change) = monitor.retained_mode.clone() else {
            return failure("mode_change_unavailable");
        };
        if change.completed
            || change.operation_id != operation_id
            || change.reference_id != reference_id
            || !change.references_match(monitor.references.as_ref())
            || !monitor.retained_mode_durable
            || monitor.state != "ready"
            || self.borrows.values().any(|borrow| borrow.key == key)
        {
            return failure("mode_change_unavailable");
        }
        if monitor.retained_mode_pending {
            return json!({"ok":true,"state":"pending","operation_id":operation_id});
        }
        if change.aborted {
            return change.response();
        }
        if !self.enqueue(Work::RetainedMode {
            key: key.clone(),
            change,
            step: crate::retained_mode::Step::Abort,
        }) {
            return failure("busy");
        }
        self.monitors
            .get_mut(&key)
            .expect("checked monitor")
            .retained_mode_pending = true;
        json!({"ok":true,"state":"pending","operation_id":operation_id})
    }
}
