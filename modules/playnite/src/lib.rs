//! Playnite owns discovery and catalog import for installed, launchable games.
mod catalog_sync;
mod configuration;
mod connector;
mod discovery;
mod focus;
mod fullscreen;
mod fullscreen_entry;
mod installation;
mod launch;
mod settings;
mod snapshot;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use configuration::ProductConfiguration;
use serde::Deserialize;
use serde_json::{Value, json};
use settings::ProductSettings;
use snapshot::Snapshot;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SERVICE: &str = "amaterasu.playnite/1";
const MANAGEMENT_CALLER: &str = "amaterasu.management";
const LAUNCH_CALLER: &str = "amaterasu.launch";
const AUTO_SYNC_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Status {},
    Refresh {},
    SettingsGet {},
    SettingsUpdate {
        enabled: bool,
        #[serde(default)]
        auto_sync: Option<bool>,
        #[serde(default)]
        sync_all_installed: Option<bool>,
        #[serde(default)]
        autosync_remove_uninstalled: Option<bool>,
        #[serde(default)]
        autosync_delete_after_days: Option<u32>,
        #[serde(default)]
        autosync_require_replacement: Option<bool>,
        #[serde(default)]
        focus_attempts: Option<u32>,
        #[serde(default)]
        focus_timeout_secs: Option<u32>,
        #[serde(default)]
        focus_exit_on_first: Option<bool>,
        #[serde(default)]
        fullscreen_entry_enabled: Option<bool>,
        #[serde(default)]
        recent_games: Option<u32>,
        #[serde(default)]
        recent_max_age_days: Option<u32>,
        #[serde(default)]
        sync_categories: Option<Vec<String>>,
        #[serde(default)]
        exclude_categories: Option<Vec<String>>,
        #[serde(default)]
        sync_plugins: Option<Vec<String>>,
        #[serde(default)]
        exclude_plugins: Option<Vec<String>>,
        #[serde(default)]
        exclude_games: Option<Vec<String>>,
        expected_revision: u64,
    },
    LaunchStart {
        operation_id: String,
        game_id: String,
    },
    LaunchLookup {
        operation_id: String,
    },
    LaunchStatus {
        ticket: String,
    },
    LaunchStop {
        ticket: String,
        exit_timeout_seconds: u64,
    },
    LaunchRecoverStop {
        operation_id: String,
        game_id: String,
        exit_timeout_seconds: u64,
    },
    FullscreenStart {
        operation_id: String,
    },
    FullscreenLookup {
        operation_id: String,
    },
    FullscreenStatus {
        ticket: String,
    },
    FullscreenStop {
        ticket: String,
        exit_timeout_seconds: u64,
    },
    FullscreenRecoverStop {
        operation_id: String,
        exit_timeout_seconds: u64,
    },
}

impl Action {
    fn is_settings(&self) -> bool {
        matches!(self, Self::SettingsGet {} | Self::SettingsUpdate { .. })
    }

    fn is_launch(&self) -> bool {
        matches!(
            self,
            Self::LaunchStart { .. }
                | Self::LaunchLookup { .. }
                | Self::LaunchStatus { .. }
                | Self::LaunchStop { .. }
                | Self::LaunchRecoverStop { .. }
        )
    }

    fn is_fullscreen_launch(&self) -> bool {
        matches!(
            self,
            Self::FullscreenStart { .. }
                | Self::FullscreenLookup { .. }
                | Self::FullscreenStatus { .. }
                | Self::FullscreenStop { .. }
                | Self::FullscreenRecoverStop { .. }
        )
    }
}

#[derive(Default)]
struct SnapshotWorker {
    receiver: Option<Receiver<Result<Snapshot, String>>>,
    _thread: Option<JoinHandle<()>>,
    settings_epoch: u64,
}

impl SnapshotWorker {
    fn start(settings_epoch: u64, processes: Vec<discovery::ProcessImage>) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let _ = sender.send(connector::request_snapshot(&processes));
        });
        Self {
            receiver: Some(receiver),
            _thread: Some(worker),
            settings_epoch,
        }
    }

    fn take_result(&mut self) -> Option<Result<Snapshot, String>> {
        let result = match self.receiver.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("Playnite snapshot worker stopped before returning a result".into())
            }
        };
        self.receiver = None;
        self._thread = None;
        Some(result)
    }

    fn busy(&self) -> bool {
        self.receiver.is_some()
    }
}

struct PlayniteModule {
    runtime: RuntimeClient,
    configuration: ProductConfiguration,
    catalog: catalog_sync::CatalogSync,
    snapshot_worker: SnapshotWorker,
    settings_epoch: u64,
    observed_settings_epoch: u64,
    next_auto_sync: Option<Instant>,
    last_error: Option<String>,
    last_snapshot_games: usize,
    last_discovery: discovery::Discovery,
    eligible_launch_ids: std::collections::BTreeSet<String>,
    launches: launch::Launches,
    fullscreen_launches: fullscreen::FullscreenSessions,
    fullscreen_entry_sync: fullscreen_entry::FullscreenEntrySync,
    next_discovery: Instant,
}

impl ManagedModule for PlayniteModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            configuration: ProductConfiguration::new(),
            catalog: catalog_sync::CatalogSync::default(),
            snapshot_worker: SnapshotWorker::default(),
            settings_epoch: 0,
            observed_settings_epoch: u64::MAX,
            next_auto_sync: None,
            last_error: None,
            last_snapshot_games: 0,
            last_discovery: discovery::Discovery::default(),
            eligible_launch_ids: std::collections::BTreeSet::new(),
            launches: launch::Launches::default(),
            fullscreen_launches: fullscreen::FullscreenSessions::default(),
            fullscreen_entry_sync: fullscreen_entry::FullscreenEntrySync::default(),
            next_discovery: Instant::now(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.configuration
            .advance(&self.runtime, &mut self.settings_epoch)?;
        self.observe_settings();
        self.launches.advance();
        self.refresh_discovery();
        self.fullscreen_launches
            .advance(Instant::now(), &self.last_discovery.processes);
        self.poll_snapshot();
        self.start_automatic_snapshot();
        self.catalog
            .advance(&self.runtime, self.settings_epoch)
            .map_err(|error| format!("Playnite catalog import failed: {error}"))?;
        self.fullscreen_entry_sync.advance(&self.runtime)?;
        if let Some(launchable_game_ids) = self.catalog.take_completed_launchable_game_ids() {
            self.eligible_launch_ids = launchable_game_ids;
        }
        match self.runtime.send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                payload,
                ..
            } => self.handle_request(id, &caller, payload),
            response => Err(format!("Playnite polling failed: {response:?}")),
        }
    }
}

impl PlayniteModule {
    fn observe_settings(&mut self) {
        if !self.configuration.ready() || self.observed_settings_epoch == self.settings_epoch {
            return;
        }
        self.observed_settings_epoch = self.settings_epoch;
        self.eligible_launch_ids.clear();
        let settings = self.configuration.settings();
        self.fullscreen_entry_sync
            .set_desired(cfg!(windows) && settings.enabled && settings.fullscreen_entry_enabled);
        self.next_auto_sync = if settings.enabled && settings.auto_sync {
            Some(Instant::now())
        } else {
            None
        };
    }

    fn refresh_discovery(&mut self) {
        refresh_discovery_at(
            &mut self.last_discovery,
            &mut self.next_discovery,
            Instant::now(),
            false,
            discovery::discover,
        );
    }

    fn refresh_discovery_for_stop(&mut self) {
        // Stop/recovery needs the current PID after a Playnite restart. The launch
        // coordinator still checks every refreshed path against the trusted install root.
        refresh_discovery_at(
            &mut self.last_discovery,
            &mut self.next_discovery,
            Instant::now(),
            true,
            discovery::discover,
        );
    }

    fn poll_snapshot(&mut self) {
        let Some(result) = self.snapshot_worker.take_result() else {
            return;
        };
        if self.snapshot_worker.settings_epoch != self.settings_epoch {
            self.last_error = Some("discarded a Playnite snapshot after settings changed".into());
            return;
        }
        match result {
            Ok(snapshot) => {
                self.last_snapshot_games = snapshot.games.len();
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |value| value.as_secs() as i64);
                match self.catalog.start(
                    &snapshot,
                    self.configuration.settings(),
                    self.settings_epoch,
                    now,
                ) {
                    Ok(()) => self.last_error = None,
                    Err(error) => self.last_error = Some(error),
                }
            }
            Err(error) => self.last_error = Some(error),
        }
    }

    fn start_automatic_snapshot(&mut self) {
        if !self.configuration.ready() {
            return;
        }
        let settings = self.configuration.settings();
        if !settings.enabled
            || !settings.auto_sync
            || self.snapshot_worker.busy()
            || self.catalog.busy()
        {
            return;
        }
        let Some(deadline) = self.next_auto_sync else {
            return;
        };
        if Instant::now() < deadline {
            return;
        }
        self.next_auto_sync = Some(Instant::now() + AUTO_SYNC_INTERVAL);
        if !self.last_discovery.running {
            self.last_error = Some("Playnite is not running".into());
            return;
        }
        self.snapshot_worker =
            SnapshotWorker::start(self.settings_epoch, self.last_discovery.processes.clone());
    }

    fn handle_request(&mut self, id: u64, caller: &str, payload: Value) -> ModuleResult<()> {
        let action = match serde_json::from_value::<Action>(payload) {
            Ok(action) => action,
            Err(_) => return self.complete(id, json!({"ok":false,"code":"invalid_request"})),
        };
        if action.is_settings() && !settings_caller_is_authorized(caller) {
            return self.complete(id, json!({"ok":false,"code":"forbidden"}));
        }
        if action.is_launch() && !launch_caller_is_authorized(caller) {
            return self.complete(id, json!({"ok":false,"code":"forbidden"}));
        }
        if action.is_fullscreen_launch() && !launch_caller_is_authorized(caller) {
            return self.complete(id, json!({"ok":false,"code":"forbidden"}));
        }
        if action.is_settings() && self.configuration.busy() {
            let code = if self.configuration.ready() {
                "settings_busy"
            } else {
                "settings_loading"
            };
            return self.complete(id, json!({"ok":false,"code":code}));
        }
        match action {
            Action::Status {} => self.complete(id, self.status()),
            Action::LaunchStart {
                operation_id,
                game_id,
            } => {
                if !self.configuration.ready() {
                    return self.complete(id, launch::not_started_error("settings_loading"));
                }
                if !self.configuration.settings().enabled {
                    return self.complete(id, launch::not_started_error("playnite_disabled"));
                }
                self.refresh_discovery();
                if !self.last_discovery.running {
                    return self.complete(id, launch::not_started_error("playnite_not_running"));
                }
                let response = self.launches.start(
                    &operation_id,
                    &game_id,
                    &self.eligible_launch_ids,
                    self.last_discovery.processes.clone(),
                    focus::FocusPolicy {
                        attempts: self.configuration.settings().focus_attempts,
                        timeout_secs: self.configuration.settings().focus_timeout_secs,
                        exit_on_first: self.configuration.settings().focus_exit_on_first,
                    },
                );
                self.complete(id, response)
            }
            Action::LaunchLookup { operation_id } => {
                let response = self.launches.lookup(&operation_id);
                self.complete(id, response)
            }
            Action::LaunchStatus { ticket } => {
                let response = self.launches.status(&ticket);
                self.complete(id, response)
            }
            Action::LaunchStop {
                ticket,
                exit_timeout_seconds,
            } => {
                self.refresh_discovery_for_stop();
                let response = self.launches.stop(
                    &ticket,
                    exit_timeout_seconds,
                    self.last_discovery.processes.clone(),
                );
                self.complete(id, response)
            }
            Action::LaunchRecoverStop {
                operation_id,
                game_id,
                exit_timeout_seconds,
            } => {
                self.refresh_discovery_for_stop();
                let response = self.launches.recover_stop(
                    &operation_id,
                    &game_id,
                    exit_timeout_seconds,
                    self.last_discovery.processes.clone(),
                );
                self.complete(id, response)
            }
            Action::FullscreenStart { operation_id } => {
                if !cfg!(windows) {
                    return self.complete(id, launch::not_started_error("unsupported_platform"));
                }
                if !self.configuration.ready() {
                    return self.complete(id, launch::not_started_error("settings_loading"));
                }
                if !self.configuration.settings().enabled {
                    return self.complete(id, launch::not_started_error("playnite_disabled"));
                }
                self.refresh_discovery_for_stop();
                let installation = installation::Installation::discover_associated();
                let response = self.fullscreen_launches.start(
                    &operation_id,
                    installation,
                    &self.last_discovery.processes,
                    focus::FocusPolicy {
                        attempts: self.configuration.settings().focus_attempts,
                        timeout_secs: self.configuration.settings().focus_timeout_secs,
                        exit_on_first: self.configuration.settings().focus_exit_on_first,
                    },
                );
                self.complete(id, response)
            }
            Action::FullscreenLookup { operation_id } => {
                let response = self.fullscreen_launches.lookup(&operation_id);
                self.complete(id, response)
            }
            Action::FullscreenStatus { ticket } => {
                let response = self.fullscreen_launches.status(&ticket);
                self.complete(id, response)
            }
            Action::FullscreenStop {
                ticket,
                exit_timeout_seconds,
            } => {
                self.refresh_discovery_for_stop();
                let response = self.fullscreen_launches.stop(
                    &ticket,
                    exit_timeout_seconds,
                    &self.last_discovery.processes,
                );
                self.complete(id, response)
            }
            Action::FullscreenRecoverStop {
                operation_id,
                exit_timeout_seconds,
            } => {
                self.refresh_discovery_for_stop();
                let response = self.fullscreen_launches.recover_stop(
                    &operation_id,
                    exit_timeout_seconds,
                    &self.last_discovery.processes,
                );
                self.complete(id, response)
            }
            Action::SettingsGet {} => {
                if !self.configuration.ready() {
                    self.complete(id, json!({"ok":false,"code":"settings_loading"}))
                } else {
                    self.complete(id, self.configuration.management_response())
                }
            }
            Action::SettingsUpdate {
                enabled,
                auto_sync,
                sync_all_installed,
                autosync_remove_uninstalled,
                autosync_delete_after_days,
                autosync_require_replacement,
                focus_attempts,
                focus_timeout_secs,
                focus_exit_on_first,
                fullscreen_entry_enabled,
                recent_games,
                recent_max_age_days,
                sync_categories,
                exclude_categories,
                sync_plugins,
                exclude_plugins,
                exclude_games,
                expected_revision,
            } => {
                let current = self.configuration.settings();
                let updated = ProductSettings {
                    enabled,
                    auto_sync: auto_sync.unwrap_or(current.auto_sync),
                    sync_all_installed: sync_all_installed.unwrap_or(current.sync_all_installed),
                    autosync_remove_uninstalled: autosync_remove_uninstalled
                        .unwrap_or(current.autosync_remove_uninstalled),
                    autosync_delete_after_days: autosync_delete_after_days
                        .unwrap_or(current.autosync_delete_after_days),
                    autosync_require_replacement: autosync_require_replacement
                        .unwrap_or(current.autosync_require_replacement),
                    focus_attempts: focus_attempts.unwrap_or(current.focus_attempts),
                    focus_timeout_secs: focus_timeout_secs.unwrap_or(current.focus_timeout_secs),
                    focus_exit_on_first: focus_exit_on_first.unwrap_or(current.focus_exit_on_first),
                    fullscreen_entry_enabled: fullscreen_entry_enabled
                        .unwrap_or(current.fullscreen_entry_enabled),
                    recent_games: recent_games.unwrap_or(current.recent_games),
                    recent_max_age_days: recent_max_age_days.unwrap_or(current.recent_max_age_days),
                    sync_categories: sync_categories
                        .unwrap_or_else(|| current.sync_categories.clone()),
                    exclude_categories: exclude_categories
                        .unwrap_or_else(|| current.exclude_categories.clone()),
                    sync_plugins: sync_plugins.unwrap_or_else(|| current.sync_plugins.clone()),
                    exclude_plugins: exclude_plugins
                        .unwrap_or_else(|| current.exclude_plugins.clone()),
                    exclude_games: exclude_games.unwrap_or_else(|| current.exclude_games.clone()),
                };
                match self
                    .configuration
                    .begin_update(id, updated, expected_revision)
                {
                    Ok(()) => Ok(()),
                    Err(response) => self.complete(id, response),
                }
            }
            Action::Refresh {} => {
                if !self.configuration.ready() {
                    return self.complete(id, json!({"ok":false,"code":"settings_loading"}));
                }
                if !self.configuration.settings().enabled {
                    return self.complete(id, json!({"ok":false,"code":"playnite_disabled"}));
                }
                if self.snapshot_worker.busy() || self.catalog.busy() {
                    return self.complete(id, json!({"ok":false,"code":"refresh_busy"}));
                }
                self.refresh_discovery();
                if !self.last_discovery.running {
                    return self.complete(id, json!({"ok":false,"code":"playnite_not_running"}));
                }
                self.snapshot_worker = SnapshotWorker::start(
                    self.settings_epoch,
                    self.last_discovery.processes.clone(),
                );
                self.complete(
                    id,
                    json!({"ok":true,"refreshing":true,"revision":self.configuration.revision()}),
                )
            }
        }
    }

    fn status(&self) -> Value {
        json!({
            "ok":true,
            "settings_ready":self.configuration.ready(),
            "enabled":self.configuration.settings().enabled,
            "auto_sync":self.configuration.settings().auto_sync,
            "fullscreen_entry_enabled":self.configuration.settings().fullscreen_entry_enabled,
            "fullscreen_supported":cfg!(windows),
            "playnite_running":self.last_discovery.running,
            "playnite_paths":self.last_discovery.executable_paths,
            "snapshot_pending":self.snapshot_worker.busy(),
            "snapshot_game_count":self.last_snapshot_games,
            "launchable_game_count":self.eligible_launch_ids.len(),
            "active_provider_launches":self.launches.active_count(),
            "active_fullscreen_sessions":self.fullscreen_launches.active_count(),
            "fullscreen_entry_sync":self.fullscreen_entry_sync.status(),
            "catalog_sync":self.catalog.status(),
            "last_error":self.last_error,
            "revision":self.configuration.revision()
        })
    }

    fn complete(&self, request: u64, payload: Value) -> ModuleResult<()> {
        match self.runtime.send(Command::Complete { request, payload })? {
            Response::Ok => Ok(()),
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                Ok(())
            }
            response => Err(format!("Playnite request completion failed: {response:?}")),
        }
    }
}

fn refresh_discovery_at(
    last_discovery: &mut discovery::Discovery,
    next_discovery: &mut Instant,
    now: Instant,
    force: bool,
    discover: impl FnOnce() -> discovery::Discovery,
) {
    if force || now >= *next_discovery {
        *last_discovery = discover();
        *next_discovery = Instant::now() + Duration::from_secs(2);
    }
}

fn settings_caller_is_authorized(caller: &str) -> bool {
    caller == MANAGEMENT_CALLER
}

fn launch_caller_is_authorized(caller: &str) -> bool {
    caller == LAUNCH_CALLER
}

amaterasu_module_sdk::export_module!(PlayniteModule);

#[cfg(test)]
mod tests;
