//! Exact capture-target inventory and caller-owned, read-only source references.
//!
//! Borrowing protects an existing output from cooperative display restoration.
//! It never authorizes creating, deleting, or reconfiguring a native output.
use crate::{Command, ModuleResult, requests::RequestOptions};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const MAX_TARGETS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub refresh_millihz: u32,
    pub hdr_enabled: bool,
}

impl Mode {
    pub fn validate(&self) -> ModuleResult<()> {
        if !(1..=32_768).contains(&self.width)
            || !(1..=32_768).contains(&self.height)
            || !(1..=1_000_000).contains(&self.refresh_millihz)
        {
            return Err("invalid_capture_target_mode".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub selector: String,
    pub label: String,
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Descriptor {
    pub fn validate(&self) -> ModuleResult<()> {
        validate_identifier(&self.selector, 256)?;
        if self.label.is_empty()
            || self.label.len() > 160
            || self.label.chars().any(char::is_control)
        {
            return Err("invalid_capture_target_label".into());
        }
        if self.ready != self.mode.is_some() || self.ready && self.error.is_some() {
            return Err("invalid_capture_target_readiness".into());
        }
        if let Some(mode) = &self.mode {
            mode.validate()?;
        }
        if let Some(error) = &self.error {
            validate_identifier(error, 128)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub output: String,
    pub mode: Mode,
    #[serde(default = "default_route")]
    pub route_key: String,
}

impl Binding {
    pub fn validate(&self) -> ModuleResult<()> {
        validate_identifier(&self.output, 512)?;
        validate_identifier(&self.route_key, 64)?;
        self.mode.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    CaptureTargets {},
    ResolveCaptureTarget {
        selector: String,
    },
    BorrowTarget {
        operation_id: String,
        selector: String,
        reference_id: String,
    },
    ReleaseTarget {
        operation_id: String,
        selector: String,
        reference_id: String,
    },
}

impl Operation {
    pub fn payload(&self) -> ModuleResult<Value> {
        match self {
            Self::CaptureTargets {} => {}
            Self::ResolveCaptureTarget { selector } => validate_identifier(selector, 256)?,
            Self::BorrowTarget {
                operation_id,
                selector,
                reference_id,
            }
            | Self::ReleaseTarget {
                operation_id,
                selector,
                reference_id,
            } => {
                validate_identifier(operation_id, 128)?;
                validate_identifier(selector, 256)?;
                validate_identifier(reference_id, 128)?;
            }
        }
        serde_json::to_value(self).map_err(|error| error.to_string())
    }
    pub fn command(&self, options: RequestOptions<'_>) -> ModuleResult<Command> {
        Ok(Command::Request {
            service: options.service.into(),
            payload: self.payload()?,
            timeout_ms: options.timeout_ms,
            operation_id: options.operation_id.into(),
            parent: options.parent,
        })
    }
}

pub fn inventory_from_reply(payload: Value) -> ModuleResult<Vec<Descriptor>> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Reply {
        ok: bool,
        targets: Vec<Descriptor>,
    }
    let reply: Reply =
        serde_json::from_value(payload).map_err(|_| "invalid_capture_target_inventory")?;
    if !reply.ok || reply.targets.len() > MAX_TARGETS {
        return Err("invalid_capture_target_inventory".into());
    }
    let mut selectors = BTreeSet::new();
    for target in &reply.targets {
        target.validate()?;
        if !selectors.insert(&target.selector) {
            return Err("ambiguous_capture_target_inventory".into());
        }
    }
    Ok(reply.targets)
}

pub fn resolved_from_reply(payload: Value, selector: &str) -> ModuleResult<Binding> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Reply {
        ok: bool,
        target: Descriptor,
        capture_output: String,
        #[serde(default = "default_route")]
        capture_route_key: String,
    }
    let reply: Reply =
        serde_json::from_value(payload).map_err(|_| "invalid_capture_target_resolution")?;
    reply.target.validate()?;
    if !reply.ok || !reply.target.ready || reply.target.selector != selector {
        return Err("capture_target_unavailable".into());
    }
    let binding = Binding {
        output: reply.capture_output,
        mode: reply.target.mode.ok_or("capture_target_unavailable")?,
        route_key: reply.capture_route_key,
    };
    binding.validate()?;
    Ok(binding)
}

pub fn borrowed_from_reply(
    payload: Value,
    selector: &str,
    reference: &str,
) -> ModuleResult<Binding> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Reply {
        ok: bool,
        selector: String,
        reference_id: String,
        binding: Binding,
    }
    let reply: Reply =
        serde_json::from_value(payload).map_err(|_| "invalid_capture_target_borrow")?;
    if !reply.ok || reply.selector != selector || reply.reference_id != reference {
        return Err("capture_target_borrow_mismatch".into());
    }
    reply.binding.validate()?;
    Ok(reply.binding)
}

pub fn released_from_reply(payload: &Value, selector: &str, reference: &str) -> bool {
    payload["ok"] == true
        && payload["released"] == true
        && payload["native_removed"] == false
        && payload["selector"] == selector
        && payload["reference_id"] == reference
}

fn validate_identifier(value: &str, maximum: usize) -> ModuleResult<()> {
    if value.is_empty()
        || value.len() > maximum
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err("invalid_capture_target_identifier".into());
    }
    Ok(())
}
fn default_route() -> String {
    "default".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn mode() -> Value {
        json!({"width":1920,"height":1080,"refresh_millihz":60_000,"hdr_enabled":false})
    }
    #[test]
    fn unchanged_consumer_validates_two_provider_bindings() {
        fn consumer(provider: impl FnOnce(Value) -> Value) -> Binding {
            let operation = Operation::ResolveCaptureTarget {
                selector: "public-output".into(),
            };
            resolved_from_reply(provider(operation.payload().unwrap()), "public-output").unwrap()
        }
        for output in ["exact-target:provider-a", "exact-target:replacement-b"] {
            let binding = consumer(|request| {
                assert_eq!(
                    request,
                    json!({"action":"resolve_capture_target","selector":"public-output"})
                );
                json!({"ok":true,"target":{"selector":"public-output","label":"A display","ready":true,"mode":mode()},"capture_output":output})
            });
            assert_eq!(binding.output, output);
        }
    }
    #[test]
    fn inventory_is_bounded_unique_and_does_not_turn_missing_mode_into_ready() {
        let target = json!({"selector":"opaque-1","label":"Display","ready":true,"mode":mode()});
        assert!(inventory_from_reply(json!({"ok":true,"targets":[target.clone()]})).is_ok());
        assert!(
            inventory_from_reply(json!({"ok":true,"targets":[target.clone(),target]})).is_err()
        );
        assert!(
            inventory_from_reply(
                json!({"ok":true,"targets":[{"selector":"opaque","label":"Display","ready":true}]})
            )
            .is_err()
        );
        assert!(inventory_from_reply(json!({"ok":true,"targets":[{"selector":"opaque","label":"Display","ready":false,"error":"capture_unavailable"}]})).is_ok());
    }
    #[test]
    fn wrong_selector_or_reference_cannot_substitute_a_borrow() {
        let payload = json!({"ok":true,"selector":"opaque","reference_id":"reference","binding":{"output":"exact-source","mode":mode()}});
        assert!(borrowed_from_reply(payload.clone(), "opaque", "reference").is_ok());
        assert!(borrowed_from_reply(payload.clone(), "different", "reference").is_err());
        assert!(borrowed_from_reply(payload, "opaque", "different").is_err());
        assert!(!released_from_reply(
            &json!({"ok":true,"released":true,"native_removed":true,"selector":"opaque","reference_id":"reference"}),
            "opaque",
            "reference"
        ));
    }
}
