//! Request contract for the opt-in NVHTTP feedback-source join.
//!
//! Startup composition selects the direct effect owner. A request is only a
//! claim by that trusted owner; the provider must independently resolve the
//! lifecycle source, private paired session, current permission and policy.
use crate::effect_authority::{Source, StreamScope};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const SERVICE: &str = "amaterasu.nvhttp/1";

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BindFeedbackSource {
    pub authority_epoch: String,
    pub source: Source,
    pub scope: StreamScope,
    pub input_boot_epoch: String,
}

impl BindFeedbackSource {
    pub fn valid(&self) -> bool {
        self.authority_epoch.len() == 32
            && self
                .authority_epoch
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            && self.source.valid()
            && self.scope.valid()
            && !self.input_boot_epoch.is_empty()
            && self.input_boot_epoch.len() <= 128
            && !self.input_boot_epoch.chars().any(char::is_control)
    }

    /// No certificate or caller identity is included in this wire request.
    pub fn payload(&self) -> Value {
        json!({"action":"bind_feedback_source","authority_epoch":self.authority_epoch,
            "source":self.source,"scope":self.scope,"input_boot_epoch":self.input_boot_epoch})
    }

    /// Stable across lost replies; unique to the lifecycle-issued source ID.
    pub fn retirement_operation_id(&self) -> String {
        format!("nvhttp-source-{}", self.source.id)
    }

    /// Query a retained uncertain join after the selected effect owner has
    /// completed source retirement through the pairing authority.
    pub fn reconciliation_payload(&self) -> Value {
        let mut payload = self.payload();
        payload["action"] = json!("reconcile_feedback_source");
        payload
    }

    pub fn submit_reconciliation(
        &self,
        runtime: &crate::RuntimeClient,
        timeout_ms: u64,
        operation_id: &str,
    ) -> crate::ModuleResult<u64> {
        if !self.valid() {
            return Err("invalid_source_join".into());
        }
        runtime.request_serialized(
            crate::requests::RequestOptions {
                service: SERVICE,
                timeout_ms,
                operation_id,
                parent: None,
            },
            &self.reconciliation_payload(),
        )
    }

    /// A successful reply is usable only for the exact requested source and scope.
    pub fn accepts_confirmation(&self, response: &Value) -> bool {
        self.valid()
            && response.as_object().is_some_and(|fields| fields.len() == 4)
            && response["ok"] == true
            && response["epoch"] == self.authority_epoch
            && response["source"] == json!(self.source)
            && response["scope"] == json!(self.scope)
    }

    /// Compare the whole-source retirement reply to the operation the owner
    /// actually retired. A new authority epoch is reported separately.
    pub fn accepts_retirement_confirmation(&self, response: &Value, operation_id: &str) -> bool {
        let current_epoch_valid = match response.get("reconciled_by") {
            None => {
                response.get("current_epoch").is_none()
                    && response.as_object().is_some_and(|fields| fields.len() == 6)
            }
            Some(value) if value == "authority_epoch" => {
                response.as_object().is_some_and(|fields| fields.len() == 8)
                    && response["current_epoch"].as_str().is_some_and(|epoch| {
                        epoch != self.authority_epoch
                            && epoch.len() == 32
                            && epoch
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    })
            }
            Some(_) => false,
        };
        self.valid()
            && !operation_id.is_empty()
            && operation_id.len() <= 128
            && !operation_id.chars().any(char::is_control)
            && response["ok"] == true
            && response["state"] == "retired"
            && response["epoch"] == self.authority_epoch
            && response["source"] == json!(self.source)
            && response["scope"] == json!(self.scope)
            && response["operation_id"] == operation_id
            && current_epoch_valid
    }

    /// Broker acceptance does not confirm that the source was bound. Retain the
    /// same operation ID when reconciling a lost admission reply.
    pub fn submit(
        &self,
        runtime: &crate::RuntimeClient,
        timeout_ms: u64,
        operation_id: &str,
    ) -> crate::ModuleResult<u64> {
        if !self.valid() {
            return Err("invalid_source_join".into());
        }
        runtime.request_serialized(
            crate::requests::RequestOptions {
                service: SERVICE,
                timeout_ms,
                operation_id,
                parent: None,
            },
            &self.payload(),
        )
    }
}
