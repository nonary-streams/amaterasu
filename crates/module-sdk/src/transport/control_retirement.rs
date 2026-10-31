//! Optional whole-control-stream retirement request. The selected transport
//! must implement this contract; an arbitrary close/status is not a receipt.
//! Only the transport owner may request it. Provider epochs and operation IDs
//! reconcile retries but are not effect-admission grants.
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Request<'a> {
    Retire {
        stream: u64,
        operation_id: &'a str,
        provider_epoch: &'a str,
    },
    RetirementStatus {
        stream: u64,
        operation_id: &'a str,
        provider_epoch: &'a str,
    },
}

impl Request<'_> {
    pub fn submit(
        &self,
        runtime: &crate::RuntimeClient,
        service: &str,
        timeout_ms: u64,
        broker_operation_id: &str,
        parent: Option<u64>,
    ) -> crate::ModuleResult<u64> {
        runtime.request_serialized(
            crate::requests::RequestOptions {
                service,
                timeout_ms,
                operation_id: broker_operation_id,
                parent,
            },
            self,
        )
    }
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub ok: bool,
    pub state: String,
    pub scope: String,
    pub stream: u64,
    pub operation_id: String,
    pub provider_epoch: String,
}

impl Receipt {
    /// Structural match only: caller must also authenticate the broker service
    /// and compose this receipt with the protocol authority's drain barrier.
    pub fn matches(&self, stream: u64, operation_id: &str, provider_epoch: &str) -> bool {
        self.ok
            && self.state == "retired"
            && self.scope == "whole_control_stream"
            && self.stream == stream
            && self.operation_id == operation_id
            && self.provider_epoch == provider_epoch
    }
}
