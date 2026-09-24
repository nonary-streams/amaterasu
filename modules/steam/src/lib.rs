//! Read-only local Steam provider. Product policy and launching stay in other libraries.
mod catalog_sync;
mod configuration;
mod discovery;
mod keyvalues;
mod settings;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use configuration::ProductConfiguration;
use discovery::{Game, Scan};
use serde::Deserialize;
use serde_json::{Value, json};
use settings::{Exclusion, ProductSettings, StartupSettings};
use std::collections::BTreeMap;
use std::path::PathBuf;

const SERVICE: &str = "amaterasu.steam/1";
const MANAGEMENT_CALLER: &str = "amaterasu.management";

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Refresh {},
    Status {},
    SettingsGet {},
    SettingsUpdate {
        enabled: bool,
        #[serde(default)]
        auto_sync: bool,
        #[serde(default)]
        exclude_games: Vec<Exclusion>,
        expected_revision: u64,
    },
    List {
        revision: Option<u64>,
        after: Option<u32>,
        limit: Option<usize>,
    },
}

#[derive(Default)]
struct Provider {
    enabled: bool,
    roots: Vec<PathBuf>,
    steam_executable: Option<PathBuf>,
    scan: Option<Scan>,
    games: BTreeMap<u32, Game>,
    revision: u64,
    snapshot_complete: bool,
    warnings: Vec<String>,
    warning_count: u64,
}

impl Provider {
    fn new(roots: Vec<PathBuf>) -> Self {
        let steam_executable = discovery::find_steam_executable(&roots);
        Self {
            roots,
            steam_executable,
            ..Self::default()
        }
    }

    fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        if !enabled {
            self.scan = None;
            self.games.clear();
            self.warnings.clear();
            self.warning_count = 0;
            self.revision = self.revision.saturating_add(1);
            self.snapshot_complete = false;
        }
    }

    fn start_scan(&mut self) -> bool {
        if !self.enabled || self.scan.is_some() || self.revision == u64::MAX {
            return false;
        }
        self.scan = Some(Scan::new(&self.roots));
        true
    }

    fn has_complete_snapshot(&self) -> bool {
        self.revision > 0 && self.snapshot_complete
    }

    fn scan_in_progress(&self) -> bool {
        self.scan.is_some()
    }

    fn tick(&mut self) {
        if !self.enabled {
            return;
        }
        if let Some(scan) = &mut self.scan {
            scan.tick();
        }
        if self.scan.as_ref().is_some_and(|scan| scan.finished) {
            let scan = self.scan.take().expect("finished scan exists");
            self.games = scan.games;
            self.warnings = scan.warnings;
            self.warning_count = scan.warning_count;
            self.snapshot_complete = scan.warning_count == 0;
            self.revision = self.revision.saturating_add(1);
        }
    }

    fn handle_action(&mut self, action: Action) -> ActionResult {
        match action {
            Action::Refresh {} => {
                if !self.enabled {
                    return ActionResult::Immediate(json!({"ok":false,"code":"steam_disabled"}));
                }
                if self.revision == u64::MAX && self.scan.is_none() {
                    return ActionResult::Immediate(
                        json!({"ok":false,"code":"revision_exhausted"}),
                    );
                }
                self.start_scan();
                ActionResult::Immediate(self.status())
            }
            Action::Status {} => ActionResult::Immediate(self.status()),
            Action::SettingsGet {} => ActionResult::SettingsGet,
            Action::SettingsUpdate {
                enabled,
                auto_sync,
                exclude_games,
                expected_revision,
            } => ActionResult::SettingsUpdate {
                settings: ProductSettings {
                    enabled,
                    auto_sync,
                    exclude_games,
                },
                expected_revision,
            },
            Action::List {
                revision,
                after,
                limit,
            } => ActionResult::Immediate(self.page(
                revision,
                after.unwrap_or(0),
                limit.unwrap_or(16),
            )),
        }
    }

    fn status(&self) -> Value {
        json!({"ok":true,"enabled":self.enabled,"scanning":self.scan.is_some(),"revision":self.revision,
            "game_count":self.games.len(),"warning_count":self.warning_count,"warnings":self.warnings,
            "complete":self.has_complete_snapshot(),
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
        json!({"ok":true,"enabled":self.enabled,"revision":self.revision,"games":games,"next_after":next_after,
            "scanning":self.scan.is_some(),"complete":self.has_complete_snapshot()})
    }
}

enum ActionResult {
    Immediate(Value),
    SettingsGet,
    SettingsUpdate {
        settings: ProductSettings,
        expected_revision: u64,
    },
}

struct SteamModule {
    runtime: RuntimeClient,
    provider: Provider,
    configuration: ProductConfiguration,
    catalog_sync: catalog_sync::CatalogSync,
}

impl ManagedModule for SteamModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let response = runtime.send(Command::GetSettings {})?;
        let Response::Result { payload } = response else {
            return Err("Steam settings unavailable".into());
        };
        let settings: StartupSettings =
            serde_json::from_value(payload).map_err(|error| error.to_string())?;
        settings.validate().map_err(str::to_owned)?;
        let configuration = ProductConfiguration::new();
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            provider: {
                let mut provider = Provider::new(settings.roots);
                provider.set_enabled(configuration.settings().enabled);
                provider
            },
            configuration,
            catalog_sync: catalog_sync::CatalogSync::new(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.provider.tick();
        self.configuration
            .advance(&self.runtime, &mut self.provider)?;
        self.catalog_sync.tick(
            &self.runtime,
            &mut self.provider,
            self.configuration.settings(),
        )?;
        match self.runtime.send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                payload,
                remaining_ms,
                ..
            } => self.handle_request(id, &caller, payload, remaining_ms),
            unexpected => Err(format!("Steam poll failed: {unexpected:?}")),
        }
    }
}

impl SteamModule {
    fn handle_request(
        &mut self,
        id: u64,
        caller: &str,
        payload: Value,
        remaining_ms: u64,
    ) -> ModuleResult<()> {
        let action = match serde_json::from_value::<Action>(payload) {
            Ok(action) => action,
            Err(_) => return self.complete(id, json!({"ok":false,"code":"invalid_request"})),
        };
        let is_settings_action = matches!(
            &action,
            Action::SettingsGet {} | Action::SettingsUpdate { .. }
        );
        if is_settings_action && !is_management_settings_caller(caller) {
            return self.complete(id, json!({"ok":false,"code":"forbidden"}));
        }
        if self.configuration.is_busy() && is_settings_action {
            let code = if self.configuration.is_ready() {
                "settings_busy"
            } else {
                "settings_loading"
            };
            return self.complete(id, json!({"ok":false,"code":code}));
        }
        if !self.configuration.is_ready() && matches!(&action, Action::Refresh {}) {
            return self.complete(id, json!({"ok":false,"code":"settings_loading"}));
        }
        match self.provider.handle_action(action) {
            ActionResult::Immediate(payload) => self.complete(
                id,
                with_catalog_sync_status(
                    with_settings_ready(payload, self.configuration.is_ready()),
                    self.catalog_sync.status(),
                ),
            ),
            ActionResult::SettingsGet => {
                self.complete(id, self.configuration.management_response())
            }
            ActionResult::SettingsUpdate {
                settings,
                expected_revision,
            } => {
                match self
                    .configuration
                    .begin_update(id, settings, expected_revision, remaining_ms)
                {
                    Ok(()) => Ok(()),
                    Err(response) => self.complete(id, response),
                }
            }
        }
    }

    fn complete(&self, request: u64, payload: Value) -> ModuleResult<()> {
        match self.runtime.send(Command::Complete { request, payload })? {
            Response::Ok => Ok(()),
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                Ok(())
            }
            response => Err(format!("Steam completion failed: {response:?}")),
        }
    }
}

fn with_settings_ready(mut payload: Value, settings_ready: bool) -> Value {
    if let Some(object) = payload.as_object_mut() {
        object.insert("settings_ready".into(), json!(settings_ready));
    }
    payload
}

fn with_catalog_sync_status(mut payload: Value, status: Value) -> Value {
    if let Some(object) = payload.as_object_mut() {
        object.insert("catalog_sync".into(), status);
    }
    payload
}

#[cfg(test)]
impl Provider {
    fn handle(&mut self, payload: Value) -> Value {
        match serde_json::from_value::<Action>(payload) {
            Ok(action) => match self.handle_action(action) {
                ActionResult::Immediate(payload) => payload,
                ActionResult::SettingsGet | ActionResult::SettingsUpdate { .. } => {
                    json!({"ok":false,"code":"management_only"})
                }
            },
            Err(_) => json!({"ok":false,"code":"invalid_request"}),
        }
    }
}

fn is_management_settings_caller(caller: &str) -> bool {
    caller == MANAGEMENT_CALLER
}

amaterasu_module_sdk::export_module!(SteamModule);

#[cfg(test)]
mod tests;
