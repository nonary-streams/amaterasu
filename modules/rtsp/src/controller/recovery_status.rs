//! Generation-bound retirement observations. Missing volatile tickets alone are
//! never evidence that a prior native capture has retired.
use super::*;

impl Controller {
    pub(super) fn retirement_status(
        &self,
        request: u64,
        caller: &str,
        operation: Option<&str>,
        expected_epoch: Option<&str>,
        runtime: &RuntimeClient,
    ) {
        let owner = if self
            .settings
            .allowed_principals
            .iter()
            .any(|principal| principal == caller)
        {
            caller
        } else if let Some(owner) = self.settings.retirement_status_owners.get(caller) {
            owner.as_str()
        } else {
            rpc::complete(runtime, request, error("permission_denied"));
            return;
        };
        if operation.is_some_and(|id| !valid_name(id))
            || expected_epoch.is_some_and(|epoch| !crate::model::valid_runtime_epoch(epoch))
        {
            rpc::complete(runtime, request, error("invalid_retirement_reference"));
            return;
        }
        // This describes the prior-runtime native reconciliation barrier.
        // Current-epoch work has separate owner-scoped occupancy observations.
        let reconciled = self.startup_recovery.ready();
        let active_operations = self
            .records
            .values()
            .filter(|record| record.owner == owner && !retirement_confirmed(record))
            .count();
        let pending_admissions = usize::from(
            self.display_admission
                .as_ref()
                .is_some_and(|admission| admission.caller == owner),
        );
        let cleanup_pending = self
            .display_cleanup
            .iter()
            .any(|cleanup| cleanup.owner == owner)
            || self.records.values().any(|record| {
                record.owner == owner
                    && !retirement_confirmed(record)
                    && matches!(record.state, State::Closing | State::CleanupFailed)
            });
        let (state, retired) = if let Some(operation) = operation {
            let record = self
                .records
                .values()
                .find(|record| record.owner == owner && record.config.operation_id == operation);
            let deferred = self.display_admission.as_ref().is_some_and(|admission| {
                admission.caller == owner && admission.config.operation_id == operation
            }) || self
                .display_cleanup
                .iter()
                .any(|cleanup| cleanup.owner == owner && cleanup.operation_id == operation);
            if deferred {
                ("active", false)
            } else if let Some(record) = record {
                let retired = retirement_confirmed(record);
                (if retired { "retired" } else { "active" }, retired)
            } else if !reconciled {
                (self.recovery_observation_state(), false)
            } else if expected_epoch.is_some_and(|epoch| epoch != self.runtime_epoch) {
                // The caller binds this proof to its durably saved server epoch.
                // Old-epoch Open requests cannot enter this instance, and startup
                // reconciliation has confirmed every prior native owner retired.
                ("epoch_fenced", true)
            } else if self
                .retired_operations
                .iter()
                .any(|(fence_owner, id)| fence_owner == owner && id == operation)
            {
                ("operation_fenced", true)
            } else {
                ("unknown", false)
            }
        } else {
            (
                if reconciled {
                    "ready"
                } else {
                    self.recovery_observation_state()
                },
                false,
            )
        };
        rpc::complete(
            runtime,
            request,
            json!({"ok":true,"runtime_epoch":self.runtime_epoch,
                "startup_reconciled":reconciled,"operation_id":operation,
                "expected_runtime_epoch":expected_epoch,
                "state":state,"retired":retired,"active_operations":active_operations,
                "pending_admissions":pending_admissions,"cleanup_pending":cleanup_pending}),
        );
    }

    fn recovery_observation_state(&self) -> &'static str {
        if self.startup_recovery.blocked() {
            "recovery_blocked"
        } else {
            "recovery_pending"
        }
    }

    pub(super) fn reconcile_startup(
        &mut self,
        request: u64,
        caller: &str,
        now: u64,
        runtime: &RuntimeClient,
    ) {
        if !self
            .settings
            .allowed_principals
            .iter()
            .any(|principal| principal == caller)
        {
            rpc::complete(runtime, request, error("permission_denied"));
            return;
        }
        if self.startup_recovery.blocked() {
            if self.startup_recovery.call.is_some()
                || !self.records.is_empty()
                || self.display_admission.is_some()
            {
                rpc::complete(runtime, request, error("startup_reconciliation_unsettled"));
                return;
            }
            let previous_sequence = self.startup_recovery.operation_sequence;
            self.startup_recovery = StartupRecovery::new(
                self.startup_recovery.nonce,
                now.saturating_add(STARTUP_RECONCILIATION_TIMEOUT_MS),
            );
            // A fresh snapshot must not reuse a broker operation whose earlier
            // result may remain cached after the failed reconciliation pass.
            self.startup_recovery.operation_sequence = previous_sequence;
        }
        self.retirement_status(request, caller, None, None, runtime);
    }

    pub(super) fn retire_operation(
        &mut self,
        request: u64,
        caller: &str,
        operation: &str,
        expected_epoch: &str,
        now: u64,
        runtime: &RuntimeClient,
    ) {
        if !self
            .settings
            .allowed_principals
            .iter()
            .any(|principal| principal == caller)
        {
            rpc::complete(runtime, request, error("permission_denied"));
            return;
        }
        if !valid_name(operation) || !crate::model::valid_runtime_epoch(expected_epoch) {
            rpc::complete(runtime, request, error("invalid_retirement_reference"));
            return;
        }
        if expected_epoch != self.runtime_epoch || !self.startup_recovery.ready() {
            self.retirement_status(
                request,
                caller,
                Some(operation),
                Some(expected_epoch),
                runtime,
            );
            return;
        }
        let fence = (caller.to_owned(), operation.to_owned());
        if !self.retired_operations.contains(&fence) && self.retired_operations.len() >= MAX_RECORDS
        {
            rpc::complete(
                runtime,
                request,
                error("retirement_fence_capacity_exhausted"),
            );
            return;
        }
        // Install the owner-bound admission fence before inspecting volatile
        // tickets. It remains for this runtime's lifetime and cannot expire in
        // front of a delayed Open. A new runtime uses another admission epoch.
        self.retired_operations.insert(fence);
        if let Some(admission) = self.display_admission.as_mut().filter(|admission| {
            admission.caller == caller && admission.config.operation_id == operation
        }) {
            admission.deadline = now;
            admission.call.cancel();
        }
        let ticket = self
            .records
            .values()
            .find(|record| {
                record.owner == caller
                    && record.config.operation_id == operation
                    && !retirement_confirmed(record)
            })
            .map(|record| record.id);
        if let Some(ticket) = ticket {
            let mut record = self
                .records
                .remove(&ticket)
                .expect("owned retirement record");
            record.close(None);
            record.cleanup_error = None;
            self.revoke_ticket(ticket);
            self.schedule_cleanup(&mut record, None, now);
            self.records.insert(ticket, record);
        }
        self.retirement_status(
            request,
            caller,
            Some(operation),
            Some(expected_epoch),
            runtime,
        );
    }
}

fn retirement_confirmed(record: &Record) -> bool {
    record.state.terminal()
        && record.cleanup_confirmed
        && record.cleanup_error.is_none()
        && record.uncertain.is_none()
        && !record.lifecycle_prepare_outstanding
}
