use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    CollectionGet {
        collection: String,
        key: Option<String>,
    },
    CollectionPage {
        collection: String,
        revision: u64,
        after: Option<String>,
        limit: usize,
    },
    CollectionCommit {
        collection: String,
        expected_revision: u64,
        metadata: Value,
        edits: Vec<crate::collection::Edit>,
    },
    CollectionDraftBegin {
        collection: String,
        draft_id: String,
        expected_revision: u64,
    },
    CollectionDraftAppend {
        collection: String,
        draft_id: String,
        sequence: u64,
        edits: Vec<crate::collection::Edit>,
    },
    CollectionDraftPublish {
        collection: String,
        draft_id: String,
        expected_revision: u64,
        metadata: Value,
    },
    CollectionDraftAbort {
        collection: String,
        draft_id: String,
    },
    Get {
        key: String,
    },
    Set {
        key: String,
        value: Value,
        expected_revision: Option<u64>,
    },
    Delete {
        key: String,
        expected_revision: Option<u64>,
    },
    Snapshot {
        cursor: Option<SnapshotCursor>,
        limit: Option<usize>,
    },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotCursor {
    pub revision: u64,
    pub after: String,
}

#[derive(Debug)]
pub struct StateError {
    code: &'static str,
    message: String,
    current_revision: Option<u64>,
}

impl StateError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            current_revision: None,
        }
    }

    pub fn conflict(current_revision: u64) -> Self {
        Self {
            code: "revision_conflict",
            message: "expected revision does not match persisted revision".into(),
            current_revision: Some(current_revision),
        }
    }

    pub fn payload(self) -> Value {
        let mut error = json!({"code": self.code, "message": self.message});
        if let Some(revision) = self.current_revision {
            error["current_revision"] = revision.into();
        }
        json!({"ok": false, "error": error})
    }
}
