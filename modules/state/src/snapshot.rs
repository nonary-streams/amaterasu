use crate::protocol::{SnapshotCursor, StateError};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const MAX_RESPONSE_BYTES: usize = 16_384;
pub const MAX_PAGE_KEYS: usize = 128;

pub fn response_fits(payload: &Value) -> bool {
    serde_json::to_vec(payload).is_ok_and(|bytes| bytes.len() <= MAX_RESPONSE_BYTES)
}

pub fn page(
    values: &BTreeMap<String, Value>,
    revision: u64,
    cursor: Option<SnapshotCursor>,
    limit: Option<usize>,
) -> Result<Value, StateError> {
    if cursor
        .as_ref()
        .is_some_and(|cursor| cursor.revision != revision)
    {
        return Err(StateError::conflict(revision));
    }
    let limit = limit.unwrap_or(MAX_PAGE_KEYS);
    let mut remaining = values
        .iter()
        .filter(|(key, _)| {
            cursor
                .as_ref()
                .is_none_or(|cursor| key.as_str() > cursor.after.as_str())
        })
        .peekable();
    let mut entries = BTreeMap::new();
    let mut response = payload(&entries, revision, None);
    while let Some((key, value)) = remaining.next() {
        entries.insert(key.clone(), value.clone());
        let next_cursor = remaining.peek().map(|_| SnapshotCursor {
            revision,
            after: key.clone(),
        });
        let candidate = payload(&entries, revision, next_cursor);
        if !response_fits(&candidate) {
            if entries.len() == 1 {
                return Err(StateError::new(
                    "value_too_large",
                    "stored value cannot fit a snapshot response",
                ));
            }
            break;
        }
        response = candidate;
        if entries.len() == limit {
            break;
        }
    }
    Ok(response)
}

pub fn validate_entry(key: &str, value: &Value) -> Result<(), StateError> {
    let entries = BTreeMap::from([(key.to_owned(), value.clone())]);
    let cursor = SnapshotCursor {
        revision: u64::MAX,
        after: key.to_owned(),
    };
    let snapshot = payload(&entries, u64::MAX, Some(cursor));
    let get = json!({"ok":true,"revision":u64::MAX,"found":true,"value":value});
    if !response_fits(&snapshot) || !response_fits(&get) {
        return Err(StateError::new(
            "value_too_large",
            "encoded value and response metadata exceed 16384 bytes",
        ));
    }
    Ok(())
}

fn payload(
    entries: &BTreeMap<String, Value>,
    revision: u64,
    next_cursor: Option<SnapshotCursor>,
) -> Value {
    json!({"ok":true,"revision":revision,"values":entries,"next_cursor":next_cursor})
}
