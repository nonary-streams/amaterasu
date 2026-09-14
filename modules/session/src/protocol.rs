use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Create {
        key: String,
    },
    Lookup {
        key: String,
    },
    Activate {
        session_id: u64,
    },
    Stop {
        session_id: u64,
    },
    FinishStop {
        session_id: u64,
    },
    Cancel {
        session_id: u64,
    },
    Fail {
        session_id: u64,
    },
    Snapshot {
        session_id: u64,
    },
    Forget {
        session_id: u64,
    },
    List {
        #[serde(default)]
        offset: usize,
        #[serde(default = "default_list_limit")]
        limit: usize,
    },
    Acquire {
        session_id: u64,
        resource: String,
        mode: LeaseMode,
    },
    Release {
        session_id: u64,
        resource: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LeaseMode {
    Shared,
    Exclusive,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Preparing,
    Active,
    Stopping,
    Stopped,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug)]
pub struct DomainError {
    pub code: &'static str,
    pub message: &'static str,
}

impl DomainError {
    pub fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
}

pub type DomainResult<T> = Result<T, DomainError>;

fn default_list_limit() -> usize {
    16
}
