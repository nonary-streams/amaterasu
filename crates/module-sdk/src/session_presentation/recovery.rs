//! Bounded recovery references; consumers invoke only their selected fixed
//! transport/execution contracts, never a service or payload chosen by a plugin.
use super::token;
use serde::{Deserialize, Serialize};

pub const MAX_RECOVERY_ITEMS: usize = 8;
pub const MAX_RECOVERY_BUDGET_MS: u64 = 30_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecoveryBinding {
    Transport {
        operation_id: String,
        epoch: String,
    },
    Application {
        operation_id: String,
        epoch: String,
        application_generation: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryWorkItem {
    /// Stable identity for this exact durable retirement intent. Providers must
    /// bind it to the original broker consumer and refuse stale work reuse.
    pub id: String,
    pub client_id: String,
    pub session_id: String,
    pub generation: u64,
    pub reservation_id: String,
    pub binding: RecoveryBinding,
    /// Bounded per-attempt budget, not permission to forget unfinished work.
    pub budget_ms: u64,
}

impl RecoveryBinding {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Transport {
                operation_id,
                epoch,
            } if token(operation_id) && token(epoch) => Ok(()),
            Self::Application {
                operation_id,
                epoch,
                application_generation,
            } if token(operation_id) && token(epoch) && *application_generation > 0 => Ok(()),
            _ => Err("invalid_recovery_binding"),
        }
    }
}

impl RecoveryWorkItem {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.binding.validate()?;
        if token(&self.id)
            && token(&self.client_id)
            && token(&self.session_id)
            && self.generation > 0
            && token(&self.reservation_id)
            && (1..=MAX_RECOVERY_BUDGET_MS).contains(&self.budget_ms)
        {
            Ok(())
        } else {
            Err("invalid_recovery_work")
        }
    }
}
