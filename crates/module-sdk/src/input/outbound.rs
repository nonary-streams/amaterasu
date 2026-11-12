//! Frozen protocol-owner agreements for optional input-provider contributions.
//!
//! These values describe an agreement; they are not proof of peer negotiation,
//! authorization, a lease, or permission to send. The session owner must obtain
//! them from its trusted protocol owner and separately validate reserved wire
//! types and provider admission. No real peer-offer protocol is defined here.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_OUTBOUND_SCHEMAS: usize = 16;
pub const MAX_OUTBOUND_SCHEMA_NAME: usize = 96;
/// Keeps one worst-case JSON-encoded event within the broker response limit.
pub const MAX_OUTBOUND_PAYLOAD: usize = 3960;
pub const MAX_OUTBOUND_RESOURCE: usize = 96;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboundAgreement {
    pub version: u16,
    pub schemas: Vec<OutboundSchema>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboundSchema {
    /// Stable, namespaced schema identifier. The owner assigns its meaning.
    pub name: String,
    pub version: u16,
    /// Inner control type, not an authenticated envelope or sequence number.
    pub message_type: u16,
}

/// Provider-produced logical message. The session supplies the wire type from
/// its approved agreement; the provider cannot select a transport or sequence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboundContribution {
    pub version: u16,
    pub schema: String,
    pub schema_version: u16,
    /// Owning session, attested by the selected provider at mailbox admission.
    pub session: u64,
    /// Provider-owned resource identity, scoped to the current input lease.
    pub resource: String,
    pub incarnation: u64,
    pub payload: Vec<u8>,
}

/// Optional contribution form of an entry returned by `feedback_peek`.
/// The provider attests the lease, session and resource incarnation before
/// exposing this event; the consumer independently checks the agreement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboundFeedbackEvent {
    pub sequence: u64,
    pub contribution: OutboundContribution,
}

impl OutboundContribution {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1
            || self.session == 0
            || !valid_schema_name(&self.schema)
            || self.schema_version == 0
            || self.resource.is_empty()
            || self.resource.len() > MAX_OUTBOUND_RESOURCE
            || !self
                .resource
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            || self.incarnation == 0
            || self.payload.len() > MAX_OUTBOUND_PAYLOAD
        {
            return Err("invalid_outbound_contribution");
        }
        Ok(())
    }
}

impl OutboundAgreement {
    /// Resolve a validated schema in this agreement, without granting admission.
    pub fn message_type(&self, contribution: &OutboundContribution) -> Option<u16> {
        contribution.validate().ok()?;
        self.schemas
            .iter()
            .find(|schema| {
                schema.name == contribution.schema && schema.version == contribution.schema_version
            })
            .map(|schema| schema.message_type)
    }

    /// Validate the bounded, unambiguous shape, not the authority of its origin.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1 || self.schemas.is_empty() || self.schemas.len() > MAX_OUTBOUND_SCHEMAS
        {
            return Err("invalid_outbound_agreement");
        }
        let mut names = BTreeSet::new();
        let mut types = BTreeSet::new();
        for schema in &self.schemas {
            if !valid_schema_name(&schema.name)
                || schema.version == 0
                || schema.message_type == 0
                || !names.insert((&schema.name, schema.version))
                || !types.insert(schema.message_type)
            {
                return Err("invalid_outbound_agreement");
            }
        }
        Ok(())
    }
}

fn valid_schema_name(name: &str) -> bool {
    name.len() <= MAX_OUTBOUND_SCHEMA_NAME
        && name.split('.').count() >= 2
        && name.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && part.as_bytes()[0].is_ascii_lowercase()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(name: &str, version: u16, message_type: u16) -> OutboundSchema {
        OutboundSchema {
            name: name.into(),
            version,
            message_type,
        }
    }

    #[test]
    fn contribution_resolves_only_its_agreed_version_and_bounds() {
        let agreement = OutboundAgreement {
            version: 1,
            schemas: vec![
                schema("org.example.alpha", 1, 0x6001),
                schema("org.example.beta", 2, 0x6002),
            ],
        };
        let mut item = OutboundContribution {
            version: 1,
            schema: "org.example.alpha".into(),
            schema_version: 1,
            session: 42,
            resource: "device-a".into(),
            incarnation: 4,
            payload: vec![1, 2],
        };
        assert_eq!(agreement.message_type(&item), Some(0x6001));
        item.schema_version = 2;
        assert_eq!(agreement.message_type(&item), None);
        item.schema = "org.example.beta".into();
        assert_eq!(agreement.message_type(&item), Some(0x6002));
        item.payload = vec![0; MAX_OUTBOUND_PAYLOAD + 1];
        assert_eq!(agreement.message_type(&item), None);
        item.payload.clear();
        item.incarnation = 0;
        assert_eq!(agreement.message_type(&item), None);
    }

    #[test]
    fn agreement_rejects_ambiguous_or_unbounded_claims() {
        let mut agreement = OutboundAgreement {
            version: 1,
            schemas: vec![
                schema("org.example.alpha", 1, 0x6001),
                schema("org.example.beta", 2, 0x6002),
            ],
        };
        assert_eq!(agreement.validate(), Ok(()));
        agreement.schemas[1].message_type = 0x6001;
        assert!(agreement.validate().is_err());
        agreement.schemas[1] = agreement.schemas[0].clone();
        agreement.schemas[1].message_type = 0x6002;
        assert!(agreement.validate().is_err());
        agreement.schemas = vec![schema("invalid", 1, 0x6001)];
        assert!(agreement.validate().is_err());
        agreement.schemas = vec![schema("org.example.alpha", 0, 0x6001)];
        assert!(agreement.validate().is_err());
        agreement.schemas = (0..=MAX_OUTBOUND_SCHEMAS)
            .map(|n| schema(&format!("org.example.s{n}"), 1, 0x6000 + n as u16))
            .collect();
        assert!(agreement.validate().is_err());
    }
}

/// Maximum JSON size of a complete broker feedback response, including the
/// envelope and every event. Providers must page their mailbox within this limit.
pub const MAX_FEEDBACK_RESPONSE_BYTES: usize = 16_384;

/// Build a broker-safe feedback response. A caller should reduce its peek page
/// when this fails; it must not acknowledge entries that were not returned.
pub fn bounded_feedback_response(
    events: &[serde_json::Value],
) -> Result<serde_json::Value, &'static str> {
    let response = serde_json::json!({"ok": true, "events": events});
    if serde_json::to_vec(&response)
        .map_err(|_| "invalid_feedback_response")?
        .len()
        > MAX_FEEDBACK_RESPONSE_BYTES
    {
        return Err("feedback_response_too_large");
    }
    Ok(response)
}

#[cfg(test)]
mod response_size_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn feedback_budget_includes_envelope_and_json_expansion() {
        let item = json!({"sequence": 7, "contribution": {"payload": vec![255u8; 4096]}});
        assert_eq!(
            bounded_feedback_response(std::slice::from_ref(&item)),
            Err("feedback_response_too_large")
        );
        let smaller = json!({"sequence": 7, "contribution": {"payload": vec![255u8; 3000]}});
        assert!(bounded_feedback_response(std::slice::from_ref(&smaller)).is_ok());
        assert_eq!(
            bounded_feedback_response(&[smaller.clone(), smaller]),
            Err("feedback_response_too_large")
        );
    }
}
