//! Versioned virtual-display provider request contract.
//!
//! Providers register [`SERVICE`] through trusted composition. Request parsing
//! does not authorize native effects: implementations must bind references to
//! the broker caller, check request admission before mutation, and retire owned
//! resources when completion or cleanup is uncertain. Responses, cancellation,
//! recovery, and capability discovery still require a shared provider adapter;
//! this request type alone does not establish full substitutability.
//!
//! A Linux provider may transfer a released final reference into a durable
//! provider-owned recovery reservation, which may remain connected as a desktop
//! anchor. Identity-specific status then reports
//! `parked: true`, `reference_owned: false` and the matching identity and
//! connector in `parked_outputs`; an idempotent remove reports `parked: true`
//! and `removed: false`. This proves reference release, not physical removal
//! or successful restoration. Consumers must preserve that distinction.
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
    /// Read-only cross-provider verification for an explicitly trusted startup
    /// verifier. The provider derives verifier authority from the broker caller,
    /// never from `owner`. This does not transfer or release the owner's lease.
    VerifyOwnerTarget {
        owner: String,
        identity: String,
        reference_id: String,
    },
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

/// Provider-neutral specification returned by trusted owner-target verification.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnedTargetSpecification {
    pub identity: String,
    pub width: u32,
    pub height: u32,
    pub refresh_millihz: u32,
    pub name: String,
    pub hdr_enabled: bool,
}

/// A read-only ownership observation, not a lease or capture-ready assertion.
/// `target_id` is an opaque display-provider selection key. Consumers must not
/// interpret it as an OS handle or derive another target from it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnerTargetProof {
    pub ok: bool,
    pub owned: bool,
    pub target_id: String,
    pub specification: OwnedTargetSpecification,
    pub active: bool,
}

/// Validate a selected verifier's response before constructing display effects.
/// The caller must additionally compare the requested complete mode and derive
/// the owner from its authenticated inbound broker request.
pub fn owner_target_from_reply(
    payload: serde_json::Value,
    expected_identity: &str,
) -> Result<OwnerTargetProof, &'static str> {
    let proof: OwnerTargetProof =
        serde_json::from_value(payload).map_err(|_| "invalid_owner_target_proof")?;
    if !proof.ok
        || !proof.owned
        || proof.specification.identity != expected_identity
        || proof.target_id.is_empty()
        || proof.target_id.len() > 4096
        || proof.target_id.chars().any(char::is_control)
        || !(320..=7680).contains(&proof.specification.width)
        || !(200..=4320).contains(&proof.specification.height)
        || !(1000..=1_000_000).contains(&proof.specification.refresh_millihz)
        || proof.specification.name.is_empty()
        || proof.specification.name.len() > 31
        || proof.specification.name.chars().any(char::is_control)
    {
        return Err("invalid_owner_target_proof");
    }
    Ok(proof)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn owner_target_proof_validates_the_exact_selected_identity_and_complete_shape() {
        let proof = json!({"ok":true,"owned":true,"target_id":"opaque-target","active":false,
            "specification":{"identity":"owned-id","width":1920,"height":1080,"refresh_millihz":60000,"name":"Monitor","hdr_enabled":false}});
        assert!(
            !owner_target_from_reply(proof.clone(), "owned-id")
                .unwrap()
                .active
        );
        assert!(owner_target_from_reply(proof.clone(), "peer-id").is_err());
        for field in ["ok", "owned"] {
            let mut invalid = proof.clone();
            invalid[field] = json!(false);
            assert!(owner_target_from_reply(invalid, "owned-id").is_err());
        }
        for invalid_target in [json!(""), json!("target\n"), json!(null)] {
            let mut invalid = proof.clone();
            invalid["target_id"] = invalid_target;
            assert!(owner_target_from_reply(invalid, "owned-id").is_err());
        }
        let mut invalid = proof;
        invalid["specification"]["width"] = json!(0);
        assert!(owner_target_from_reply(invalid, "owned-id").is_err());
    }

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
