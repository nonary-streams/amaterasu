use amaterasu_module_sdk::extension_api::CaptureBinding;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub type CaptureResult<T> = Result<T, &'static str>;

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub selection_principals: BTreeSet<String>,
    pub capture_principals: BTreeSet<String>,
    pub idle_timeout_seconds: u32,
    pub selection_timeout_seconds: u32,
    pub remember_source: bool,
    pub allow_named_outputs: bool,
    /// Separately approved physical/existing desktop output capture. The old
    /// named-output approval remains restricted to managed Virtual-N outputs.
    pub allow_desktop_outputs: bool,
    pub frame_fit_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            selection_principals: BTreeSet::new(),
            capture_principals: BTreeSet::new(),
            idle_timeout_seconds: 60,
            selection_timeout_seconds: 120,
            remember_source: false,
            allow_named_outputs: false,
            allow_desktop_outputs: false,
            frame_fit_enabled: false,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> CaptureResult<()> {
        if !(1..=600).contains(&self.idle_timeout_seconds)
            || !(1..=300).contains(&self.selection_timeout_seconds)
            || !valid_principals(&self.selection_principals)
            || !valid_principals(&self.capture_principals)
        {
            return Err("invalid_settings");
        }
        Ok(())
    }
}

fn valid_principals(principals: &BTreeSet<String>) -> bool {
    principals.len() <= 16
        && principals.iter().all(|name| {
            !name.is_empty()
                && name.len() <= 128
                && name.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'-' | b'_' | b'/')
                })
        })
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    Monitor,
    Window,
}

impl SourceType {
    pub fn mask(self) -> u32 {
        match self {
            Self::Monitor => 1,
            Self::Window => 2,
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Status,
    Enumerate {
        offset: Option<usize>,
        limit: Option<usize>,
    },
    SelectSource {
        source_type: SourceType,
        operation_id: String,
    },
    PollSelection {
        operation_id: String,
    },
    CancelSelection {
        operation_id: String,
    },
    Open {
        output: String,
        operation_id: Option<String>,
        constant_rate: Option<bool>,
    },
    #[serde(skip)]
    OpenBound {
        output: String,
        operation_id: Option<String>,
        constant_rate: Option<bool>,
        binding: Box<CaptureBinding>,
    },
    Lookup {
        operation_id: String,
    },
    Close {
        stream: u64,
    },
}

impl Operation {
    pub fn open_target(&self) -> Option<(&str, Option<&str>)> {
        match self {
            Self::Open {
                output,
                operation_id,
                ..
            }
            | Self::OpenBound {
                output,
                operation_id,
                ..
            } => Some((output, operation_id.as_deref())),
            _ => None,
        }
    }

    pub fn named_operation(&self) -> Option<&str> {
        self.open_target()
            .filter(|(output, _)| output.starts_with("kwin:"))
            .and_then(|(_, operation)| operation)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KwinOutputSelector {
    output: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundOpen {
    action: String,
    operation_id: Option<String>,
    constant_rate: Option<bool>,
    source_format: Option<Value>,
    capture_binding: Value,
}

pub fn decode_operation(payload: Value) -> CaptureResult<Operation> {
    if payload.get("capture_binding").is_some() {
        let request: BoundOpen =
            serde_json::from_value(payload).map_err(|_| "invalid_capture_binding")?;
        if request.action != "open" {
            return Err("invalid_capture_binding");
        }
        let binding =
            amaterasu_module_sdk::capture::decode_capture_binding(request.capture_binding)?;
        if binding.capture_provider != crate::SERVICE
            || binding.selector_schema != "amaterasu.kwin-wl-output/1"
        {
            return Err("capture_selector_unsupported");
        }
        if binding.mode.hdr || request.source_format.is_some() {
            return Err("capture_source_format_unsupported");
        }
        let selector: KwinOutputSelector = serde_json::from_value(binding.selector.clone())
            .map_err(|_| "invalid_capture_selector")?;
        if !selector
            .output
            .strip_prefix("kwin:")
            .is_some_and(valid_owned_name)
        {
            return Err("invalid_capture_selector");
        }
        return Ok(Operation::OpenBound {
            output: selector.output,
            operation_id: request.operation_id,
            constant_rate: request.constant_rate,
            binding: Box::new(binding),
        });
    }
    serde_json::from_value(payload).map_err(|_| "invalid_request")
}

pub(crate) fn valid_owned_name(name: &str) -> bool {
    name.strip_prefix("Virtual-")
        .filter(|number| !number.starts_with('0') && number.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|number| number.parse::<u32>().ok())
        .is_some_and(|number| number > 0)
}

#[cfg(test)]
mod managed_capture_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stock_binding_selects_only_the_exact_managed_output() {
        let request = json!({"action":"open","operation_id":"exact-open","capture_binding":{
            "capture_provider":crate::SERVICE,"selector_schema":"amaterasu.kwin-wl-output/1",
            "selector":{"output":"kwin:Virtual-12"},
            "resource":{"provider":"display","resource_id":"handle","incarnation":"one",
                "revision":1,"execution_domain":"seat","mutation_domain":null},
            "mode":{"width":1280,"height":720,"refresh_millihz":60000,"hdr":false},
            "plan_revision":1,"geometry_revision":1,"generation":1
        }});
        assert!(matches!(decode_operation(request.clone()),
            Ok(Operation::OpenBound { output, .. }) if output == "kwin:Virtual-12"));
        for name in [
            "kwin:DP-1",
            "kwin:Virtual-0",
            "portal-selected-session-source",
        ] {
            let mut invalid = request.clone();
            invalid["capture_binding"]["selector"]["output"] = json!(name);
            assert_eq!(
                decode_operation(invalid).err(),
                Some("invalid_capture_selector")
            );
        }
        let mut unsupported = request.clone();
        unsupported["capture_binding"]["selector_schema"] = json!("vendor.selector/1");
        assert_eq!(
            decode_operation(unsupported).err(),
            Some("capture_selector_unsupported")
        );
        let mut unsupported = request;
        unsupported["source_format"] = json!({"bit_depth":10,"dynamic_range":"sdr"});
        assert_eq!(
            decode_operation(unsupported).err(),
            Some("capture_source_format_unsupported")
        );
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Availability {
    pub native_supported: bool,
    pub available: bool,
    pub portal_version: u32,
    pub source_types: u32,
    pub cursor_modes: u32,
    pub reason: Option<&'static str>,
}

impl Availability {
    #[cfg(any(not(amaterasu_pipewire), test))]
    pub fn unsupported(reason: &'static str) -> Self {
        Self {
            native_supported: false,
            available: false,
            portal_version: 0,
            source_types: 0,
            cursor_modes: 0,
            reason: Some(reason),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceState {
    Selecting,
    Ready { node: u32, width: u32, height: u32 },
    Denied,
    Failed(&'static str),
    Revoked,
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub struct SourceDiagnostics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<SourceStage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipewire_node: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipewire_serial: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipewire_state: Option<PipeWireState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipewire_error: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter_error_code: Option<u32>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceStage {
    ConnectingSessionBus,
    CreateSession,
    SelectSources,
    Start,
    #[serde(rename = "open_pipewire_remote")]
    OpenPipeWireRemote,
    #[serde(rename = "connect_pipewire")]
    ConnectPipeWire,
    WaitingForFrames,
    FrameReceived,
}

impl SourceStage {
    pub fn from_native(value: u32) -> Option<Self> {
        Some(match value {
            1 => Self::ConnectingSessionBus,
            2 => Self::CreateSession,
            3 => Self::SelectSources,
            4 => Self::Start,
            5 => Self::OpenPipeWireRemote,
            6 => Self::ConnectPipeWire,
            7 => Self::WaitingForFrames,
            8 => Self::FrameReceived,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PipeWireState {
    Unconnected,
    Connecting,
    Paused,
    Streaming,
    Error,
}

impl PipeWireState {
    pub fn from_native(value: i32) -> Option<Self> {
        Some(match value {
            0 => Self::Unconnected,
            1 => Self::Connecting,
            2 => Self::Paused,
            3 => Self::Streaming,
            -1 => Self::Error,
            _ => return None,
        })
    }
}

pub fn validate_operation_id(id: &str) -> CaptureResult<()> {
    if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
        Err("invalid_operation_id")
    } else {
        Ok(())
    }
}

pub fn new_id() -> CaptureResult<u64> {
    let mut bytes = [0; 8];
    getrandom::fill(&mut bytes).map_err(|_| "entropy_error")?;
    let value = u64::from_le_bytes(bytes);
    if value == 0 {
        Err("entropy_error")
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod diagnostics_tests {
    use super::*;

    #[test]
    fn native_diagnostic_codes_map_to_safe_status_tokens() {
        assert_eq!(SourceStage::from_native(4), Some(SourceStage::Start));
        assert_eq!(
            SourceStage::from_native(5),
            Some(SourceStage::OpenPipeWireRemote)
        );
        assert_eq!(
            SourceStage::from_native(6),
            Some(SourceStage::ConnectPipeWire)
        );
        assert_eq!(SourceStage::from_native(99), None);
        assert_eq!(PipeWireState::from_native(-1), Some(PipeWireState::Error));
        assert_eq!(PipeWireState::from_native(i32::MAX), None);
    }
}
