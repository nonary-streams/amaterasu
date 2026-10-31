//! Versioned virtual-display provider request contract.
//!
//! Providers register [`SERVICE`] through trusted composition. Request parsing
//! does not authorize native effects: implementations must bind references to
//! the broker caller, check request admission before mutation, and retire owned
//! resources when completion or cleanup is uncertain. Responses, cancellation,
//! recovery, and capability discovery still require a shared provider adapter;
//! this request type alone does not establish full substitutability.
use serde::{Deserialize, Deserializer, Serialize};

pub const SERVICE: &str = "amaterasu.virtual-display/1";

/// How an owned virtual output joins the desktop, named after Vibeshine's
/// `virtual_display_layout`. Its isolated and primary-only variants are not
/// implemented and are refused when settings are parsed.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VirtualDisplayLayout {
    /// Every other display is turned off for the stream and restored after it.
    #[default]
    Exclusive,
    /// The other displays stay on and the owned output extends the desktop.
    Extended,
}

fn present_integer<'de, D>(deserializer: D) -> Result<Option<i32>, D::Error>
where
    D: Deserializer<'de>,
{
    i32::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    ReconfigureModeOnlyStatus {
        operation_id: String,
        identity: String,
        reference_id: String,
        borrow_id: String,
        expected_topology_domain_id: String,
        expected_topology_revision: u64,
    },
    ReconfigureModeOnly {
        operation_id: String,
        identity: String,
        reference_id: String,
        borrow_id: String,
        width: u32,
        height: u32,
        refresh_millihz: u32,
        expected_topology_domain_id: String,
        expected_topology_revision: u64,
    },
    ReconfigureStatus {
        operation_id: String,
        identity: String,
        reference_id: String,
        borrow_id: String,
        expected_topology_domain_id: String,
        expected_topology_revision: u64,
    },
    Reconfigure {
        operation_id: String,
        identity: String,
        reference_id: String,
        borrow_id: String,
        width: u32,
        height: u32,
        refresh_millihz: u32,
        expected_topology_domain_id: String,
        expected_topology_revision: u64,
    },
    Borrow {
        identity: String,
        reference_id: String,
    },
    ValidateBorrow {
        identity: String,
        reference_id: String,
        borrow_id: String,
    },
    ReleaseBorrow {
        identity: String,
        reference_id: String,
        borrow_id: String,
    },
    Create {
        #[serde(default)]
        hdr_enabled: bool,
        identity: String,
        width: u32,
        height: u32,
        refresh_millihz: u32,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        virtual_display_layout: Option<VirtualDisplayLayout>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_topology_revision: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_topology_domain_id: Option<String>,
        /// Stable caller-scoped reference for shared monitor ownership.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reference_id: Option<String>,
        /// Overrides the module's `dd_virtual_display_scale` for this output.
        /// Omitted means the module setting; an explicit null is invalid.
        #[serde(
            default,
            deserialize_with = "present_integer",
            skip_serializing_if = "Option::is_none"
        )]
        dd_virtual_display_scale: Option<i32>,
    },
    Status {
        identity: Option<String>,
        /// Ask whether this specific session still owns a shared monitor reference.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reference_id: Option<String>,
    },
    Remove {
        identity: String,
        /// Release only this session's reference when supplied.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reference_id: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn optional_scale_preserves_omission_and_rejects_explicit_null() {
        let request = Operation::Create {
            hdr_enabled: false,
            identity: "00000000-0000-0000-0000-000000000001".into(),
            width: 1920,
            height: 1080,
            refresh_millihz: 60000,
            name: "Display".into(),
            virtual_display_layout: None,
            expected_topology_revision: None,
            expected_topology_domain_id: None,
            reference_id: Some("session-1".into()),
            dd_virtual_display_scale: None,
        };
        let mut wire = serde_json::to_value(request).unwrap();
        assert!(wire.get("dd_virtual_display_scale").is_none());
        assert!(serde_json::from_value::<Operation>(wire.clone()).is_ok());
        wire["dd_virtual_display_scale"] = json!(null);
        assert!(serde_json::from_value::<Operation>(wire).is_err());
    }
}
