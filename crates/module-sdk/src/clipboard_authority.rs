//! Typed stream reservation contract for clipboard request lifetimes.
//! A reservation is issued by lifecycle, not by a client or input device. It
//! must be combined with the selected pairing authority's directional permit.
use crate::effect_authority::ClipboardStream;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum LifecycleRequest {
    ClipboardStreamStatus {},
    ReserveClipboardStream {
        operation_id: String,
        source: ClipboardStream,
        sequence: u64,
    },
    LookupClipboardStream {
        operation_id: String,
    },
    CompleteClipboardStream {
        operation_id: String,
        ticket: String,
    },
    AcknowledgeClipboardStream {
        operation_id: String,
        ticket: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ReservationState {
    Reserved,
    Completed,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Reservation {
    pub version: u8,
    pub status: ReservationState,
    pub operation_id: String,
    pub source: ClipboardStream,
    pub ticket: String,
}
impl Reservation {
    /// Match all trusted request identities, never just a successful status.
    pub fn matches(&self, operation: &str, source: &ClipboardStream) -> bool {
        self.version == 1
            && self.operation_id == operation
            && self.source == *source
            && source.valid()
            && self.ticket.len() == 32
            && self
                .ticket
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipt_requires_exact_stream_operation_and_well_formed_ticket() {
        let source = ClipboardStream {
            version: 1,
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 4,
        };
        let mut receipt = Reservation {
            version: 1,
            status: ReservationState::Reserved,
            operation_id: "op".into(),
            source: source.clone(),
            ticket: "a".repeat(32),
        };
        assert!(receipt.matches("op", &source));
        assert_eq!(source.operation_id(0), None);
        assert_eq!(source.operation_id(1), source.operation_id(1));
        assert_ne!(source.operation_id(1), source.operation_id(2));
        assert!(!receipt.matches("other", &source));
        let mut other = source.clone();
        other.generation += 1;
        assert!(!receipt.matches("op", &other));
        receipt.ticket = "A".repeat(32);
        assert!(!receipt.matches("op", &source));
    }
}
