//! Input-side retirement handshake. A successful response is accepted only from
//! the trusted selected stream service and only for the exact retired binding.
use crate::{Command, ModuleResult, Response, effect_authority::StreamRetirement};
use amaterasu_extension_api::LifecycleTarget;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// Bounded input-owner handshake for whole-control-stream retirement.
/// Keep the binding fenced until `advance` returns true.
pub struct Job {
    service: String,
    token: String,
    target: LifecycleTarget,
    input_boot_epoch: String,
    operation_id: String,
    pending: Option<u64>,
    next_attempt: Instant,
}
impl Job {
    pub fn new(
        service: String,
        token: String,
        target: LifecycleTarget,
        input_boot_epoch: String,
        operation_id: String,
    ) -> Result<Self, &'static str> {
        if operation_id.is_empty()
            || operation_id.len() > 128
            || operation_id.chars().any(char::is_control)
        {
            return Err("invalid_feedback_retirement_operation");
        }
        Ok(Self {
            service,
            token,
            target,
            input_boot_epoch,
            operation_id,
            pending: None,
            next_attempt: Instant::now(),
        })
    }
    pub fn advance(&mut self, send: &mut impl FnMut(Command) -> ModuleResult<Response>) -> bool {
        if let Some(id) = self.pending {
            match send(Command::TakeResult { request: id }) {
                Ok(Response::Pending) => return false,
                Ok(Response::Result { payload }) => {
                    // If releasing the result is uncertain keep the handle and
                    // retry; a lost release must not turn into early success.
                    if !matches!(
                        send(Command::ReleaseResult { request: id }),
                        Ok(Response::Ok)
                    ) {
                        return false;
                    }
                    self.pending = None;
                    if self.matches(&payload) {
                        return true;
                    }
                }
                _ => {
                    let _ = send(Command::ReleaseResult { request: id });
                    self.pending = None;
                }
            }
            self.next_attempt = Instant::now() + Duration::from_secs(1);
            return false;
        }
        if Instant::now() < self.next_attempt {
            return false;
        }
        let response = send(Command::Request {
            service: self.service.clone(),
            payload: json!({"action":"retire_input_feedback","version":1,"operation_id":self.operation_id,"token":self.token,"target":self.target,"input_boot_epoch":self.input_boot_epoch}),
            timeout_ms: 5000,
            operation_id: self.operation_id.clone(),
            parent: None,
        });
        match response {
            Ok(Response::Accepted { id }) => self.pending = Some(id),
            _ => self.next_attempt = Instant::now() + Duration::from_secs(1),
        }
        false
    }
    /// Check the owner request and a bounded whole-control-stream receipt.
    pub fn matches(&self, value: &Value) -> bool {
        let Some(object) = value.as_object() else {
            return false;
        };
        if object.len() != 9
            || value["ok"] != true
            || value["status"] != "retired"
            || value["pairing_retired"] != true
            || value["source_count"]
                .as_u64()
                .is_none_or(|count| count > 64)
            || value["token"] != self.token
            || value["target"] != json!(self.target)
            || value["input_boot_epoch"] != self.input_boot_epoch
            || value["operation_id"] != self.operation_id
        {
            return false;
        }
        let Ok(receipt) = serde_json::from_value::<StreamRetirement>(value["retirement"].clone())
        else {
            return false;
        };
        receipt.ok
            && receipt.state == "retired"
            && receipt.scope == "whole_control_stream"
            && receipt.stream != 0
            && receipt.provider_epoch.len() == 32
            && receipt
                .provider_epoch
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && !receipt.service.is_empty()
            && receipt.service.ends_with("/1")
            && !receipt.operation_id.is_empty()
            && receipt.operation_id.len() <= 128
    }
}
