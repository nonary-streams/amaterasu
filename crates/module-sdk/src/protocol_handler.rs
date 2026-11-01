//! Bounded broker messages for a protocol owner invoking a selected handler.
//!
//! These values do not authenticate their sender. A handler must trust the
//! broker grant and protocol owner that supplied them, not client-provided JSON.
//! The owner verifies the peer and authorization before invocation, rechecks
//! them as appropriate, and controls deadlines, cancellation and delivery.
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

/// Broker payloads must fit the runtime's 16 KiB JSON payload limit.
pub const MAX_MESSAGE_BYTES: usize = 16_384;
pub const MAX_BODY_BYTES: usize = 4_096;
pub const MAX_CERTIFICATE_BYTES: usize = 8_192;
pub const MAX_TARGET_BYTES: usize = 2_048;

/// Version 1 request supplied only by the protocol owner after verification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    /// Composition-selected schema, never chosen by the remote client.
    pub schema: String,
    pub method: String,
    pub target: String,
    pub body_base64: String,
    pub peer: VerifiedPeer,
}

/// The certificate is public DER from the verified connection, not a key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedPeer {
    pub remote: IpAddr,
    pub certificate_der_base64: String,
}

/// The protocol owner validates this before converting it into a wire reply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub version: u32,
    pub status: u16,
    pub content_type: String,
    pub body_base64: String,
}

/// Owner-created channel IDs and exact negotiated descriptions sent only to
/// the broker-selected handler. IDs are not credentials and are never sent to
/// the remote HTTP client. Native access still checks the runtime principal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpgradeOffer {
    pub version: u32,
    pub request: Request,
    pub transfer_service: String,
    pub input_id: crate::data_plane::abi::ChannelId,
    pub input: crate::data_plane::abi::Description,
    pub output_id: crate::data_plane::abi::ChannelId,
    pub output: crate::data_plane::abi::Description,
}

/// A distinct handler result, accepted only for a configured upgrade binding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpgradeDecision {
    Accept { version: u32 },
    Reject { version: u32 },
}

impl UpgradeDecision {
    pub fn accepted(&self) -> Option<bool> {
        match self {
            Self::Accept { version: 1 } => Some(true),
            Self::Reject { version: 1 } => Some(false),
            _ => None,
        }
    }
}

impl UpgradeOffer {
    pub fn validate(&self) -> bool {
        use crate::data_plane::{
            self,
            abi::{SINK, SOURCE},
        };
        self.version == 1
            && self.request.validate()
            && amaterasu_contracts::valid_name(&self.transfer_service)
            && self.input_id.incarnation != 0
            && self.input_id.sequence != 0
            && self.output_id.incarnation != 0
            && self.output_id.sequence != 0
            && self.input_id != self.output_id
            && self.input_id.incarnation == self.output_id.incarnation
            && self.input.direction == SOURCE
            && self.output.direction == SINK
            && self.input.limits.max_metadata_bytes == 0
            && self.output.limits.max_metadata_bytes == 0
            && data_plane::description(
                SOURCE,
                self.input.format,
                self.input.metadata_schema,
                self.input.limits,
            ) == Ok(self.input)
            && data_plane::description(
                SINK,
                self.output.format,
                self.output.metadata_schema,
                self.output.limits,
            ) == Ok(self.output)
            && fits_message(self)
    }
}

fn bounded_base64(value: &str, limit: usize, empty_allowed: bool) -> bool {
    if value.len() > limit.div_ceil(3) * 4 {
        return false;
    }
    STANDARD.decode(value).is_ok_and(|bytes| {
        bytes.len() <= limit
            && (empty_allowed || !bytes.is_empty())
            && STANDARD.encode(bytes) == value
    })
}

fn fits_message<T: Serialize>(value: &T) -> bool {
    serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= MAX_MESSAGE_BYTES)
}

impl Request {
    pub fn validate(&self) -> bool {
        self.version == 1
            && !self.schema.is_empty()
            && self.schema.len() <= 128
            && self
                .schema
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
            && !self.method.is_empty()
            && self.method.len() <= 16
            && self.method.bytes().all(|b| b.is_ascii_uppercase())
            && self.target.starts_with('/')
            && self.target.len() <= MAX_TARGET_BYTES
            && !self
                .target
                .bytes()
                .any(|b| b.is_ascii_control() || b == b' ')
            && bounded_base64(&self.body_base64, MAX_BODY_BYTES, true)
            && bounded_base64(
                &self.peer.certificate_der_base64,
                MAX_CERTIFICATE_BYTES,
                false,
            )
            && fits_message(self)
    }
}

impl Response {
    pub fn validate(&self) -> bool {
        self.version == 1
            && (200..=599).contains(&self.status)
            && !self.content_type.is_empty()
            && self.content_type.len() <= 128
            && self
                .content_type
                .bytes()
                .all(|b| (0x20..=0x7e).contains(&b))
            && bounded_base64(&self.body_base64, MAX_BODY_BYTES, true)
            && fits_message(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_version_bounds_and_canonical_encoding() {
        let mut request = Request {
            version: 1,
            schema: "example.neutral/1".into(),
            method: "POST".into(),
            target: "/example?a=b".into(),
            body_base64: STANDARD.encode(vec![1; MAX_BODY_BYTES]),
            peer: VerifiedPeer {
                remote: "127.0.0.1".parse().unwrap(),
                certificate_der_base64: STANDARD.encode(vec![2; 2_048]),
            },
        };
        assert!(request.validate());
        request.body_base64 = STANDARD.encode(vec![1; MAX_BODY_BYTES + 1]);
        assert!(!request.validate());
        request.body_base64 = "AQ".into();
        assert!(!request.validate());
        request.body_base64.clear();
        request.version = 2;
        assert!(!request.validate());
    }

    #[test]
    fn upgrade_decision_is_distinct_and_rejects_unknown_versions() {
        assert_eq!(
            serde_json::from_str::<UpgradeDecision>(r#"{"outcome":"accept","version":1}"#)
                .unwrap()
                .accepted(),
            Some(true)
        );
        assert_eq!(
            serde_json::from_str::<UpgradeDecision>(r#"{"outcome":"reject","version":1}"#)
                .unwrap()
                .accepted(),
            Some(false)
        );
        assert!(
            serde_json::from_str::<UpgradeDecision>(
                r#"{"version":1,"status":200,"content_type":"text/plain","body_base64":""}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<UpgradeDecision>(r#"{"outcome":"accept","version":2}"#)
                .unwrap()
                .accepted()
                .is_none()
        );
        assert!(
            serde_json::from_str::<UpgradeDecision>(r#"{"outcome":"other","version":1}"#).is_err()
        );
    }

    #[test]
    fn rejects_malformed_responses() {
        let mut reply = Response {
            version: 1,
            status: 200,
            content_type: "application/octet-stream".into(),
            body_base64: STANDARD.encode(b"hello"),
        };
        assert!(reply.validate());
        reply.content_type = "text/plain\r\nInjected: x".into();
        assert!(!reply.validate());
        reply.content_type = "text/plain".into();
        reply.status = 101;
        assert!(!reply.validate());
        reply.status = 200;
        reply.body_base64 = STANDARD.encode(vec![0; MAX_BODY_BYTES + 1]);
        assert!(!reply.validate());
    }
}
