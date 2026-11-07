//! Acknowledge bounded input bookkeeping only after native retirement is durable.
use super::*;
use amaterasu_module_sdk::input::{
    INPUT_BINDING_RETIREMENT_VERSION, INPUT_SERVICE, InputBindingRetirement, InputRequest,
    binding_retirement_acknowledged, binding_retirement_from_reply,
};
use std::time::{Duration, Instant};

const RETRY_DELAY: Duration = Duration::from_millis(100);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct AcknowledgementJob {
    operation_id: String,
    request: Option<u64>,
    result: Option<Value>,
    deadline: Instant,
    retry_at: Instant,
}

impl AcknowledgementJob {
    fn new() -> ModuleResult<Self> {
        let now = Instant::now();
        Ok(Self {
            operation_id: identifier()?,
            request: None,
            result: None,
            deadline: now + REQUEST_TIMEOUT,
            retry_at: now,
        })
    }

    fn retry(&mut self) -> ModuleResult<()> {
        self.operation_id = identifier()?;
        self.request = None;
        self.result = None;
        self.retry_at = Instant::now() + RETRY_DELAY;
        self.deadline = self.retry_at + REQUEST_TIMEOUT;
        Ok(())
    }
}

pub(super) fn receipt_from_result(
    journal: &Journal,
    result: &Value,
    token: &str,
    target: &LifecycleTarget,
    revoke_operation_id: &str,
) -> ModuleResult<Option<InputBindingRetirement>> {
    let Some(advertised) = result.get("retirement_receipt") else {
        return Ok(None);
    };
    let version = advertised
        .get("version")
        .and_then(Value::as_u64)
        .ok_or("invalid_input_binding_retirement")?;
    if version != u64::from(INPUT_BINDING_RETIREMENT_VERSION) {
        return Ok(None);
    }
    let expected = InputBindingRetirement {
        version: INPUT_BINDING_RETIREMENT_VERSION,
        token: token.into(),
        target: target.clone(),
        revoke_operation_id: revoke_operation_id.into(),
        input_boot_epoch: journal
            .input_boot_epochs
            .get(token)
            .cloned()
            .or_else(|| {
                // A revoke may reach the input provider before its pending
                // acquisition validates. The authenticated exact retirement
                // can identify that provider instance without inventing an
                // acquisition epoch that was never persisted.
                advertised["input_boot_epoch"]
                    .as_str()
                    .filter(|epoch| valid_id(epoch))
                    .map(str::to_owned)
            })
            .ok_or("input_retirement_boot_epoch_missing")?,
    };
    binding_retirement_from_reply(result, &expected).map_err(str::to_owned)
}

pub(super) fn result_released(response: ModuleResult<Response>) -> bool {
    matches!(response, Ok(Response::Ok))
        || matches!(response, Ok(Response::Error { code, .. }) if code == "not_found")
}

impl Coordinator {
    pub(super) fn advance_input_retirement_acknowledgements(
        &mut self,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        let pending: Vec<_> = self
            .shared
            .borrow()
            .value
            .inputs
            .pending_retirements()
            .cloned()
            .collect();
        if pending.is_empty() {
            return Ok(());
        }
        let start = self.input_retirement_cursor % pending.len();
        let receipts = (0..pending.len().min(8))
            .map(|offset| pending[(start + offset) % pending.len()].clone());
        self.input_retirement_cursor = (start + 8) % pending.len();
        for receipt in receipts {
            if !self.input_retirement_jobs.contains_key(&receipt.token) {
                self.input_retirement_jobs
                    .insert(receipt.token.clone(), AcknowledgementJob::new()?);
            }
            let job = self
                .input_retirement_jobs
                .get_mut(&receipt.token)
                .expect("pending acknowledgement");
            if Instant::now() < job.retry_at {
                continue;
            }
            if let Some(result) = job.result.as_ref() {
                if let Some(request) = job.request {
                    if !result_released(send(Command::ReleaseResult { request })) {
                        continue;
                    }
                    job.request = None;
                }
                if binding_retirement_acknowledged(result, &receipt) {
                    let mut next = self.shared.borrow().value.clone();
                    next.inputs
                        .acknowledge_retirement(&receipt)
                        .map_err(|_| "input_retirement_ack_conflict")?;
                    // Keep the exact acknowledged result until this journal
                    // transaction succeeds, even if its broker result is gone.
                    save(&self.shared, next)?;
                    self.input_retirement_jobs.remove(&receipt.token);
                } else {
                    job.retry()?;
                }
            } else if let Some(request) = job.request {
                if Instant::now() >= job.deadline {
                    // This is idempotent metadata, never native cleanup. Retain
                    // the exact request until its cancellation/result settles.
                    let _ = send(Command::Cancel { request });
                }
                match send(Command::TakeResult { request }) {
                    Ok(Response::Pending) | Err(_) => {}
                    Ok(Response::Result { payload }) => job.result = Some(payload),
                    Ok(Response::Error { code, .. }) if code == "not_found" => job.retry()?,
                    Ok(_) => job.result = Some(Value::Null),
                }
            } else {
                match send(Command::Request {
                    service: INPUT_SERVICE.into(),
                    payload: InputRequest::AcknowledgeBindingRetirement { receipt: &receipt }
                        .payload(),
                    timeout_ms: REQUEST_TIMEOUT.as_millis() as u64,
                    operation_id: format!("input-retirement-ack-{}", job.operation_id),
                    parent: None,
                }) {
                    Ok(Response::Accepted { id }) => {
                        job.request = Some(id);
                        job.deadline = Instant::now() + REQUEST_TIMEOUT;
                    }
                    Err(_) => {
                        // Admission may have succeeded before its local reply
                        // was lost. Retry this same immutable broker operation.
                        job.retry_at = Instant::now() + RETRY_DELAY;
                    }
                    Ok(_) => job.retry()?,
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
