//! Caller-owned, target-only display composition.
//!
//! Trusted composition selects the display and virtual-display providers. The
//! display provider verifies that the broker caller owns the named monitor
//! reference before it changes its exact target; all observed peers are retained.
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub mod targets;

pub const SERVICE: &str = "amaterasu.display/1";

/// Schema-compatible target configuration. IDs are opaque provider selectors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub refresh: Refresh,
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub rotation: Rotation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr_enabled: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Refresh {
    pub numerator: u32,
    pub denominator: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rotation {
    #[default]
    Identity,
    Rotate90,
    Rotate180,
    Rotate270,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScopedOperation {
    ScopedApply {
        operation_id: String,
        identity: String,
        reference_id: String,
        target: Target,
        expected_topology_revision: String,
        #[serde(default)]
        primary: bool,
    },
    ScopedRelease {
        operation_id: String,
        identity: String,
        reference_id: String,
        expected_topology_revision: String,
    },
}

impl ScopedOperation {
    pub fn payload(&self) -> Result<Value, String> {
        serde_json::to_value(self).map_err(|error| error.to_string())
    }
}
