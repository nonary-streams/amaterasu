//! Read-only application lifetime evidence for independently retained sources.
use super::*;

pub(super) enum RetirementAction {
    Fenced(Value),
    Stop(u64),
}

impl Pipeline {
    pub fn set_application_retirement_observers(&mut self, observers: BTreeMap<String, String>) {
        self.application_retirement_observers = observers;
    }

    pub(super) fn decode_operation(
        &self,
        service: &str,
        payload: Value,
        caller: &str,
        initiator: &str,
    ) -> Result<Operation, &'static str> {
        let operation = crate::library::decode(service, payload, caller, initiator)?;
        if self.application_retirement_observers.contains_key(caller)
            && !matches!(operation, Operation::ApplicationRetirementStatus { .. })
        {
            return Err("permission_denied");
        }
        Ok(operation)
    }

    pub(super) fn application_retirement_status(
        &self,
        caller: &str,
        operation_id: &str,
        expected_runtime_epoch: &str,
    ) -> Value {
        let owner = self
            .application_retirement_observers
            .get(caller)
            .map_or(caller, String::as_str);
        let (state, retired) = if expected_runtime_epoch != self.records.operation_epoch() {
            // A new admission epoch prevents stale opens. It does not prove
            // old native processes or preparation undos have retired. Launch
            // has no durable native recovery journal at this contract version.
            ("epoch_fenced", false)
        } else if self.records.is_tombstoned(owner, operation_id)
            || self.records.native_retired(owner, operation_id)
        {
            ("retired", true)
        } else {
            match self.records.owned_operation(owner, operation_id) {
                Ok(record)
                    if record.native_retirement_confirmed
                        && record.state.terminal()
                        && record.cleanup_error.is_none()
                        && record.outstanding_request.is_none() =>
                {
                    ("retired", true)
                }
                Ok(_) => ("active", false),
                Err(_) => ("unknown", false),
            }
        };
        serde_json::json!({
            "ok":true,
            "operation_id":operation_id,
            "expected_runtime_epoch":expected_runtime_epoch,
            "runtime_epoch":self.records.operation_epoch(),
            "startup_reconciled":false,
            "state":state,
            "retired":retired,
        })
    }

    pub(super) fn retire_operation(
        &mut self,
        caller: &str,
        operation_id: &str,
        expected_runtime_epoch: &str,
    ) -> Result<RetirementAction, &'static str> {
        if expected_runtime_epoch != self.records.operation_epoch() {
            return Err("stale_operation_epoch");
        }
        let fenced = || {
            serde_json::json!({"ok":true,"operation_id":operation_id,
            "runtime_epoch":expected_runtime_epoch,"retired":true,"fenced":true})
        };
        if self.records.is_tombstoned(caller, operation_id)
            || self.records.native_retired(caller, operation_id)
            || self
                .records
                .tombstone_unstarted(caller, operation_id)
                .is_ok()
        {
            return Ok(RetirementAction::Fenced(fenced()));
        }
        match self.records.owned_operation(caller, operation_id) {
            Ok(record) => Ok(RetirementAction::Stop(record.launch_id)),
            Err(_) => {
                self.records.tombstone_absent(caller, operation_id)?;
                Ok(RetirementAction::Fenced(fenced()))
            }
        }
    }

    pub(super) fn begin_retirement_cleanup(
        &mut self,
        caller: &str,
        launch_id: u64,
        request: u64,
        job: &mut Workflow,
    ) -> Result<Option<Value>, &'static str> {
        let record = self.records.owned(caller, launch_id)?;
        if record.state.terminal() {
            return Ok(Some(record.response()));
        }
        let outcome = if record.state == State::CleanupFailed {
            record.cleanup_outcome
        } else {
            Outcome::Completed
        };
        let record = self
            .records
            .values
            .get_mut(&launch_id)
            .expect("owned launch record");
        record.display_admission = None;
        if let Some(pending) = job.pending.take() {
            record.outstanding_request = Some(pending);
        }
        job.begin_cleanup(record, outcome, None);
        job.request = Some(request);
        Ok(None)
    }
}
