use crate::model::{Document, Entry, MAX_ENTRIES, bounded_text, parse_id};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Create {
        entry: Entry,
        expected_revision: u64,
    },
    Get {
        id: String,
    },
    ResolveLaunch {
        id: String,
    },
    ResolveExportedLaunch {
        id: String,
    },
    Update {
        id: String,
        entry: Entry,
        expected_revision: u64,
    },
    Delete {
        id: String,
        expected_revision: u64,
    },
    List {
        cursor: Option<Cursor>,
        limit: Option<usize>,
    },
    ExportList {
        cursor: Option<Cursor>,
        limit: Option<usize>,
    },
    Search {
        query: String,
        cursor: Option<Cursor>,
        limit: Option<usize>,
    },
    /// Provider snapshots are authorized from the Poll caller and the catalog's
    /// fixed startup import map. The request has no catalog owner selector.
    ProviderSnapshotBegin {
        sync_id: String,
        source_revision: u64,
    },
    ProviderSnapshotBatch {
        sync_id: String,
        generation: u64,
        source_revision: u64,
        sequence: u64,
        expected_revision: u64,
        entries: Vec<ProviderEntry>,
    },
    ProviderSnapshotFinish {
        sync_id: String,
        generation: u64,
        source_revision: u64,
        expected_revision: u64,
        next_sequence: u64,
        complete: bool,
        #[serde(default = "default_remove_stale")]
        remove_stale: bool,
    },
}

fn default_remove_stale() -> bool {
    true
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderEntry {
    pub source_id: String,
    pub entry: Entry,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub revision: u64,
    pub after: String,
    pub query: String,
}

pub enum Change {
    Read(Value),
    Write { document: Document, reply: Value },
}

pub fn apply(operation: Operation, existing: Option<Document>) -> Result<Change, Value> {
    let mut document = existing.unwrap_or_default();
    let reply = match operation {
        Operation::Get { id }
        | Operation::ResolveLaunch { id }
        | Operation::ResolveExportedLaunch { id } => {
            let entry = find_entry(&document, &id)?;
            return Ok(Change::Read(
                json!({"ok":true,"revision":document.revision,"id":id,"entry":entry}),
            ));
        }
        Operation::List { cursor, limit } => return page(&document, "", cursor, limit, false),
        Operation::ExportList { cursor, limit } => return page(&document, "", cursor, limit, true),
        Operation::Search {
            query,
            cursor,
            limit,
        } => {
            if !bounded_text(&query, 160) || query.trim().is_empty() {
                return Err(error(
                    "invalid_request",
                    "search query must contain 1..160 printable bytes",
                ));
            }
            return page(&document, &query.to_lowercase(), cursor, limit, false);
        }
        Operation::ProviderSnapshotBegin { .. }
        | Operation::ProviderSnapshotBatch { .. }
        | Operation::ProviderSnapshotFinish { .. } => {
            return Err(error(
                "invalid_request",
                "provider snapshots are handled by the catalog pipeline",
            ));
        }
        Operation::Create {
            entry,
            expected_revision,
        } => {
            check_revision(&document, expected_revision)?;
            entry
                .validate()
                .map_err(|message| error("invalid_entry", message))?;
            if document.entries.len() >= MAX_ENTRIES {
                return Err(error("resource_exhausted", "catalog entry limit reached"));
            }
            let id = format!("app-{:016x}", document.next_id);
            document.next_id = document
                .next_id
                .checked_add(1)
                .ok_or_else(|| error("resource_exhausted", "catalog ID space exhausted"))?;
            let reply = json!({"id":id,"entry":entry});
            document.entries.insert(id, entry);
            reply
        }
        Operation::Update {
            id,
            entry,
            expected_revision,
        } => {
            check_revision(&document, expected_revision)?;
            find_entry(&document, &id)?;
            entry
                .validate()
                .map_err(|message| error("invalid_entry", message))?;
            let reply = json!({"id":id,"entry":entry});
            document.entries.insert(id, entry);
            reply
        }
        Operation::Delete {
            id,
            expected_revision,
        } => {
            check_revision(&document, expected_revision)?;
            find_entry(&document, &id)?;
            document.entries.remove(&id);
            json!({"id":id,"deleted":true})
        }
    };
    document.revision = document
        .revision
        .checked_add(1)
        .ok_or_else(|| error("resource_exhausted", "catalog revision exhausted"))?;
    let mut reply = reply;
    reply["ok"] = true.into();
    reply["revision"] = document.revision.into();
    Ok(Change::Write { document, reply })
}

fn find_entry<'a>(document: &'a Document, id: &str) -> Result<&'a Entry, Value> {
    if parse_id(id).is_none() {
        return Err(error("invalid_request", "invalid catalog ID"));
    }
    document
        .entries
        .get(id)
        .ok_or_else(|| error("not_found", "catalog entry does not exist"))
}

fn check_revision(document: &Document, expected: u64) -> Result<(), Value> {
    if expected != document.revision {
        return Err(
            json!({"ok":false,"error":{"code":"revision_conflict","message":"catalog revision differs","current_revision":document.revision}}),
        );
    }
    Ok(())
}

fn page(
    document: &Document,
    query: &str,
    cursor: Option<Cursor>,
    limit: Option<usize>,
    export: bool,
) -> Result<Change, Value> {
    let limit = limit.unwrap_or(20);
    if !(1..=32).contains(&limit) {
        return Err(error(
            "invalid_request",
            "page limit must be between 1 and 32",
        ));
    }
    let after = if let Some(cursor) = &cursor {
        check_revision(document, cursor.revision)?;
        if cursor.query != query || !document.entries.contains_key(&cursor.after) {
            return Err(error(
                "invalid_cursor",
                "cursor does not match this query or catalog",
            ));
        }
        cursor.after.as_str()
    } else {
        ""
    };
    let mut matching = document
        .entries
        .iter()
        .filter(|(id, entry)| id.as_str() > after && matches_query(entry, query));
    let entries: Vec<Value> = matching
        .by_ref()
        .take(limit)
        .map(|(id, entry)| project_entry(id, entry, export))
        .collect();
    let next_cursor = if matching.next().is_some() {
        entries.last().map(|entry| Cursor {
            revision: document.revision,
            after: entry["id"].as_str().unwrap_or_default().into(),
            query: query.into(),
        })
    } else {
        None
    };
    Ok(Change::Read(
        json!({"ok":true,"revision":document.revision,"entries":entries,"next_cursor":next_cursor}),
    ))
}

pub fn project_entry(id: &str, entry: &Entry, export: bool) -> Value {
    if export {
        json!({"id":id,"name":entry.name})
    } else {
        json!({"id":id,"entry":entry})
    }
}

pub fn matches_query(entry: &Entry, query: &str) -> bool {
    query.is_empty()
        || entry.name.to_lowercase().contains(query)
        || entry.description.to_lowercase().contains(query)
        || entry
            .tags
            .iter()
            .any(|tag| tag.to_lowercase().contains(query))
}

pub fn error(code: &str, message: impl Into<String>) -> Value {
    let message: String = message.into().chars().take(512).collect();
    json!({"ok":false,"error":{"code":code,"message":message}})
}

#[cfg(test)]
mod tests;
