//! Read-only local Steam provider. Product policy and launching stay in other libraries.
mod discovery;
mod keyvalues;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use discovery::{Game, Scan};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

const SERVICE: &str = "amaterasu.steam/1";

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    roots: Vec<PathBuf>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Refresh {},
    Status {},
    List {
        revision: Option<u64>,
        after: Option<u32>,
        limit: Option<usize>,
    },
}

#[derive(Default)]
struct Provider {
    roots: Vec<PathBuf>,
    scan: Option<Scan>,
    games: BTreeMap<u32, Game>,
    revision: u64,
    warnings: Vec<String>,
    warning_count: u64,
}

impl Provider {
    fn tick(&mut self) {
        if let Some(scan) = &mut self.scan {
            scan.tick();
        }
        if self.scan.as_ref().is_some_and(|scan| scan.finished) {
            let scan = self.scan.take().expect("finished scan exists");
            self.games = scan.games;
            self.warnings = scan.warnings;
            self.warning_count = scan.warning_count;
            self.revision += 1;
        }
    }

    fn handle(&mut self, payload: Value) -> Value {
        let action = match serde_json::from_value(payload) {
            Ok(action) => action,
            Err(_) => return json!({"ok":false,"code":"invalid_request"}),
        };
        match action {
            Action::Refresh {} => {
                if self.scan.is_none() {
                    if self.revision == u64::MAX {
                        return json!({"ok":false,"code":"revision_exhausted"});
                    }
                    self.scan = Some(Scan::new(&self.roots));
                }
                self.status()
            }
            Action::Status {} => self.status(),
            Action::List {
                revision,
                after,
                limit,
            } => self.page(revision, after.unwrap_or(0), limit.unwrap_or(16)),
        }
    }

    fn status(&self) -> Value {
        json!({"ok":true,"scanning":self.scan.is_some(),"revision":self.revision,
            "game_count":self.games.len(),"warning_count":self.warning_count,"warnings":self.warnings,
            "complete":self.revision > 0 && self.warning_count == 0,
            "visited_entries":self.scan.as_ref().map_or(0, |scan| scan.visited_entries)})
    }

    fn page(&self, revision: Option<u64>, after: u32, limit: usize) -> Value {
        if !(1..=32).contains(&limit) || (after != 0 && revision.is_none()) {
            return json!({"ok":false,"code":"invalid_cursor"});
        }
        if revision.is_some_and(|revision| revision != self.revision) {
            return json!({"ok":false,"code":"revision_conflict","revision":self.revision});
        }
        let mut games = Vec::new();
        let mut more = false;
        for (_, game) in self
            .games
            .range((std::ops::Bound::Excluded(after), std::ops::Bound::Unbounded))
        {
            games.push(game);
            // Reserve metadata bytes as well as worst-case JSON escaping in records.
            if games.len() > limit
                || serde_json::to_vec(&games).map_or(true, |bytes| bytes.len() > 14_000)
            {
                games.pop();
                more = true;
                break;
            }
        }
        let next_after = if more {
            games.last().map(|game| game.app_id)
        } else {
            None
        };
        json!({"ok":true,"revision":self.revision,"games":games,"next_after":next_after,
            "scanning":self.scan.is_some(),"complete":self.revision > 0 && self.warning_count == 0})
    }
}

struct SteamModule {
    runtime: RuntimeClient,
    provider: Provider,
}

impl ManagedModule for SteamModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let response = runtime.send(Command::GetSettings {})?;
        let Response::Result { payload } = response else {
            return Err("Steam settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|error| error.to_string())?;
        if settings.roots.len() > 32
            || settings
                .roots
                .iter()
                .any(|root| !root.is_absolute() || root.as_os_str().len() > 4096)
        {
            return Err("Steam roots must be at most 32 bounded absolute paths".into());
        }
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            provider: Provider {
                roots: settings.roots,
                ..Provider::default()
            },
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.provider.tick();
        match self.runtime.send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request { id, payload, .. } => {
                let payload = self.provider.handle(payload);
                match self.runtime.send(Command::Complete {
                    request: id,
                    payload,
                })? {
                    Response::Ok => Ok(()),
                    Response::Error { code, .. }
                        if code == "request_ended" || code == "not_found" =>
                    {
                        Ok(())
                    }
                    unexpected => Err(format!("Steam completion failed: {unexpected:?}")),
                }
            }
            unexpected => Err(format!("Steam poll failed: {unexpected:?}")),
        }
    }
}

amaterasu_module_sdk::export_module!(SteamModule);
