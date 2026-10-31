//! Optional pairing-owned admission contract for source-scoped effects.
//!
//! The selected effect owner must hold each ticket until its final local effect
//! or send completes. A source identifier is not a grant. This contract alone
//! does not authenticate a protocol-to-lifecycle join or implement a barrier.
use serde::{Deserialize, Serialize};

pub const SERVICE: &str = "amaterasu.pairing-effects/1";

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq, Ord, PartialOrd)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub version: u8,
    pub id: String,
    pub generation: u64,
    pub host_epoch: String,
    pub session_id: String,
    pub stream_id: String,
    pub stream_generation: u64,
    pub device: String,
    pub device_generation: u64,
    pub effect_owner_epoch: String,
}

impl Source {
    pub fn valid(&self) -> bool {
        self.version == 1
            && hex(&self.id, 32)
            && token(&self.host_epoch)
            && token(&self.session_id)
            && token(&self.stream_id)
            && self.generation != 0
            && self.generation == self.stream_generation
            && token(&self.device)
            && self.device_generation != 0
            && token(&self.effect_owner_epoch)
    }
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn token(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

/// Broker request shape for the optional, startup-selected authority service.
/// Bind is an assertion by the trusted protocol component, never client JSON.
#[derive(Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Recover {
        epoch: String,
        owner_epoch: String,
    },
    Bind {
        epoch: String,
        certificate_pem: String,
        source: Source,
    },
    Admit {
        epoch: String,
        source: Source,
        operation_id: String,
        sequence: u64,
    },
    Status {},
    Drain {
        epoch: String,
        fence_generation: u64,
    },
    Lookup {
        epoch: String,
        operation_id: String,
    },
    Acknowledge {
        epoch: String,
        operation_id: String,
        ticket: String,
    },
    Complete {
        epoch: String,
        operation_id: String,
        ticket: String,
    },
}

/// Owner-side ticket lifecycle; does not authenticate a source or activate routes.
pub mod consumer;
