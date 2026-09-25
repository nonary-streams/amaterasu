use crate::model::{ConfigOverrides, Document, Entry, Launch, MAX_ENTRIES, bounded_text, parse_id};
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
    /// Launch uses this authorized, read-only resolution for preflight. Its
    /// returned entry and revision must be captured together before launch;
    /// a later catalog read may refer to a different application revision.
    ResolveExportedLaunch {
        id: String,
    },
    Update {
        id: String,
        entry: Entry,
        expected_revision: u64,
    },
    UpdateProviderOverrides {
        id: String,
        expected_revision: u64,
        config_overrides: ConfigOverrides,
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
    /// Export only Steam IDs from the owner selected by steam_id_export_principals.
    ExportSteamIds {
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
        #[serde(default)]
        expected_collection_revision: Option<u64>,
    },
    /// Reads the authorized importer's provenance without exposing an owner
    /// selector. Each page is pinned to the state collection revision.
    ProviderSnapshotInventory {
        after: Option<String>,
        expected_collection_revision: Option<u64>,
        limit: Option<usize>,
        #[serde(default)]
        include_entry_names: bool,
    },
    ProviderSnapshotBatch {
        sync_id: String,
        generation: u64,
        source_revision: u64,
        sequence: u64,
        expected_revision: u64,
        entries: Vec<ProviderEntry>,
        #[serde(default)]
        remove_source_ids: Vec<String>,
        #[serde(default)]
        expire_source_ids: Vec<String>,
        #[serde(default)]
        selection_remove_source_ids: Vec<String>,
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
        #[serde(default)]
        remove_uninstalled: bool,
        #[serde(default)]
        remove_expired: bool,
        #[serde(default)]
        remove_selection: bool,
    },
    /// Reconcile Playnite's one reserved synthetic Fullscreen entry. The
    /// importer identity and catalog owner come from the Poll caller and the
    /// fixed startup import map; callers cannot choose either value.
    ProviderPlayniteFullscreenEntry {
        enabled: bool,
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
    #[serde(default)]
    pub source_flags: Option<u32>,
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
        Operation::ExportSteamIds { cursor, limit } => {
            return page_steam_ids(&document, cursor, limit);
        }
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
        | Operation::ProviderSnapshotInventory { .. }
        | Operation::ProviderSnapshotBatch { .. }
        | Operation::ProviderSnapshotFinish { .. }
        | Operation::ProviderPlayniteFullscreenEntry { .. }
        | Operation::UpdateProviderOverrides { .. } => {
            return Err(error(
                "invalid_request",
                "provider-owned operations are handled by the catalog pipeline",
            ));
        }
        Operation::Create {
            entry,
            expected_revision,
        } => {
            check_revision(&document, expected_revision)?;
            reject_provider_artwork(&entry)?;
            entry
                .validate()
                .map_err(|message| error("invalid_entry", message))?;
            if matches!(&entry.launch, Launch::Playnite { .. }) {
                return Err(error(
                    "provider_launch_required",
                    "Playnite launch targets can only be written by the trusted provider",
                ));
            }
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
            reject_provider_artwork(&entry)?;
            let existing = find_entry(&document, &id)?;
            if matches!(&existing.launch, Launch::Playnite { .. }) {
                return Err(error(
                    "provider_entry_managed",
                    "Playnite launch identity can only be changed by its provider",
                ));
            }
            entry
                .validate()
                .map_err(|message| error("invalid_entry", message))?;
            if matches!(&entry.launch, Launch::Playnite { .. }) {
                return Err(error(
                    "provider_launch_required",
                    "Playnite launch targets can only be written by the trusted provider",
                ));
            }
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

fn reject_provider_artwork(entry: &Entry) -> Result<(), Value> {
    if entry.artwork.is_some() {
        return Err(error(
            "provider_artwork_required",
            "provider artwork can only be written by its authenticated provider",
        ));
    }
    Ok(())
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
    let mut reply =
        json!({"ok":true,"revision":document.revision,"entries":entries,"next_cursor":next_cursor});
    if !export {
        reply["config_override_capabilities"] = config_override_capabilities();
    }
    Ok(Change::Read(reply))
}

pub(crate) fn config_override_capabilities() -> Value {
    let frame_limiter_provider = if cfg!(windows) {
        json!(["auto", "rtss", "nvidia-control-panel", "none"])
    } else if cfg!(target_os = "linux") {
        json!(["auto", "mangohud", "proton", "mangohud-proton", "none"])
    } else {
        json!(["auto"])
    };
    let rtss_frame_limit_type = if cfg!(windows) {
        json!([
            "async",
            "front edge sync",
            "back edge sync",
            "nvidia reflex"
        ])
    } else {
        json!([])
    };

    json!({
        "frame_limiter_provider": frame_limiter_provider,
        "rtss_frame_limit_type": rtss_frame_limit_type
    })
}

fn page_steam_ids(
    document: &Document,
    cursor: Option<Cursor>,
    limit: Option<usize>,
) -> Result<Change, Value> {
    const CURSOR_QUERY: &str = "steam_ids";

    let limit = limit.unwrap_or(20);
    if !(1..=32).contains(&limit) {
        return Err(error(
            "invalid_request",
            "page limit must be between 1 and 32",
        ));
    }
    let after = if let Some(cursor) = &cursor {
        check_revision(document, cursor.revision)?;
        if cursor.query != CURSOR_QUERY
            || !document.entries.contains_key(&cursor.after)
            || crate::model::parse_id(&cursor.after).is_none()
        {
            return Err(error(
                "invalid_cursor",
                "cursor does not match this query or catalog",
            ));
        }
        cursor.after.as_str()
    } else {
        ""
    };

    let mut matching = document.entries.iter().filter_map(|(entry_id, entry)| {
        if entry_id.as_str() <= after {
            return None;
        }
        let steam_id_text = entry.steam_id.as_deref()?;
        if !crate::model::valid_steam_id(steam_id_text) {
            return None;
        }
        let steam_id = steam_id_text.parse::<u32>().ok()?;
        (steam_id != 0).then_some((entry_id, steam_id))
    });
    let mut selected = matching.by_ref().take(limit + 1).collect::<Vec<_>>();
    let has_more = selected.len() > limit;
    selected.truncate(limit);
    let next_cursor = has_more.then(|| Cursor {
        revision: document.revision,
        after: selected
            .last()
            .expect("a full ID page has at least one item")
            .0
            .clone(),
        query: CURSOR_QUERY.into(),
    });
    let steam_ids = selected
        .into_iter()
        .map(|(_, steam_id)| steam_id)
        .collect::<Vec<_>>();

    Ok(Change::Read(json!({
        "ok":true,
        "revision":document.revision,
        "steam_ids":steam_ids,
        "next_cursor":next_cursor
    })))
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
