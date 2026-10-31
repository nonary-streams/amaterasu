//! Independent snapshot library. Storage and cursors belong to this plugin;
//! execution targets are opaque references, not catalog records or commands.
use amaterasu_module_sdk::application_library::v2::{
    Capabilities, Cursor, Entry, LaunchTarget, Page, Query, Readiness,
    adapter::{CallerPolicy, Identity, Library, LibraryAdapter},
};
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    service: String,
    callers: BTreeMap<String, CallerPolicy>,
    revision: String,
    entries: Vec<Entry>,
    targets: Vec<LaunchTarget>,
}

struct Continuation {
    caller: String,
    initiator: String,
    query: Query,
    after: String,
}

struct SnapshotLibrary {
    revision: String,
    entries: BTreeMap<String, Entry>,
    targets: BTreeMap<String, LaunchTarget>,
    cursors: BTreeMap<String, Continuation>,
    next_cursor: u64,
}

impl SnapshotLibrary {
    fn new(
        revision: String,
        entries: Vec<Entry>,
        targets: Vec<LaunchTarget>,
    ) -> Result<Self, &'static str> {
        // Validate the snapshot token without imposing numeric IDs/revisions.
        Page {
            revision: revision.clone(),
            entries: vec![],
            next_cursor: None,
        }
        .validate_for(None, 1)?;
        if entries.len() > 1024 || targets.len() > entries.len() {
            return Err("invalid_snapshot");
        }
        let mut indexed = BTreeMap::new();
        for entry in entries {
            entry.validate()?;
            if indexed.insert(entry.id.clone(), entry).is_some() {
                return Err("duplicate_id");
            }
        }
        let mut executions = BTreeMap::new();
        for target in targets {
            let entry = indexed.get(&target.id).ok_or("missing_entry")?;
            target.validate_for(&entry.id, &entry.revision)?;
            if executions.insert(target.id.clone(), target).is_some() {
                return Err("duplicate_target");
            }
        }
        Ok(Self {
            revision,
            entries: indexed,
            targets: executions,
            cursors: BTreeMap::new(),
            next_cursor: 0,
        })
    }

    fn continuation(
        &self,
        identity: &Identity<'_>,
        query: &Query,
        cursor: Option<&Cursor>,
    ) -> Result<Option<&str>, &'static str> {
        let Some(cursor) = cursor else {
            return Ok(None);
        };
        if cursor.revision != self.revision {
            return Err("stale_cursor");
        }
        let continuation = self.cursors.get(&cursor.token).ok_or("invalid_cursor")?;
        if continuation.caller != identity.caller
            || continuation.initiator != identity.initiator
            || &continuation.query != query
        {
            return Err("invalid_cursor");
        }
        Ok(Some(&continuation.after))
    }
}

impl Library for SnapshotLibrary {
    fn capabilities(&self) -> Capabilities {
        use amaterasu_module_sdk::application_library::v2::{RESOLVE_LAUNCH, SEARCH};
        let mut features = std::collections::BTreeSet::from([SEARCH.into()]);
        if !self.targets.is_empty() {
            features.insert(RESOLVE_LAUNCH.into());
        }
        Capabilities {
            version: 2,
            features,
        }
    }

    fn list(
        &mut self,
        identity: &Identity<'_>,
        query: &Query,
        cursor: Option<&Cursor>,
        limit: usize,
    ) -> Result<Page, &'static str> {
        let after = self.continuation(identity, query, cursor)?;
        let search = query.text.to_lowercase();
        let mut entries: Vec<_> = self
            .entries
            .values()
            .filter(|entry| {
                after.is_none_or(|after| entry.id.as_str() > after)
                    && (entry.name.to_lowercase().contains(&search)
                        || entry
                            .tags
                            .iter()
                            .any(|tag| tag.to_lowercase().contains(&search)))
            })
            .take(limit + 1)
            .cloned()
            .collect();
        let next_cursor = if entries.len() > limit {
            entries.pop();
            // Bound retained cursors and expire the oldest. A client with an
            // expired token gets invalid_cursor and restarts its query.
            if self.cursors.len() >= 128 {
                let oldest = self.next_cursor - 127;
                self.cursors.remove(&format!("page-{oldest}"));
            }
            self.next_cursor = self.next_cursor.checked_add(1).ok_or("cursor_capacity")?;
            let token = format!("page-{}", self.next_cursor);
            self.cursors.insert(
                token.clone(),
                Continuation {
                    caller: identity.caller.into(),
                    initiator: identity.initiator.into(),
                    query: query.clone(),
                    after: entries.last().ok_or("invalid_limit")?.id.clone(),
                },
            );
            Some(Cursor {
                revision: self.revision.clone(),
                token,
            })
        } else {
            None
        };
        Ok(Page {
            revision: self.revision.clone(),
            entries,
            next_cursor,
        })
    }

    fn resolve(&mut self, _: &Identity<'_>, id: &str) -> Result<Entry, &'static str> {
        self.entries.get(id).cloned().ok_or("not_found")
    }

    fn resolve_launch(
        &mut self,
        _: &Identity<'_>,
        id: &str,
        revision: &str,
    ) -> Result<LaunchTarget, &'static str> {
        let entry = self.entries.get(id).ok_or("not_found")?;
        if entry.revision != revision {
            return Err("revision_conflict");
        }
        if entry.readiness != Readiness::Ready {
            return Err("not_ready");
        }
        self.targets.get(id).cloned().ok_or("not_launchable")
    }
}

pub struct Module {
    runtime: RuntimeClient,
    adapter: LibraryAdapter<SnapshotLibrary>,
}

impl ManagedModule for Module {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("library settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|error| error.to_string())?;
        let library = SnapshotLibrary::new(settings.revision, settings.entries, settings.targets)
            .map_err(str::to_owned)?;
        let adapter = LibraryAdapter::new(library, settings.service.clone(), settings.callers)
            .map_err(str::to_owned)?;
        runtime.require_success(Command::Register {
            service: settings.service,
        })?;
        Ok(Self { runtime, adapter })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        if let Some(request) = self.runtime.poll_serialized()? {
            let response = self.adapter.dispatch(&request);
            match self.runtime.send(Command::Complete {
                request: request.id,
                payload: response,
            })? {
                Response::Ok => {}
                Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {}
                response => return Err(format!("library completion failed: {response:?}")),
            }
        }
        Ok(())
    }
}

#[cfg(all(not(test), not(feature = "embedded")))]
amaterasu_module_sdk::export_module!(Module);

#[cfg(test)]
mod tests;
