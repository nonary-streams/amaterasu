//! Linux Lutris discovery, product settings, and catalog import provider.

mod catalog_sync;
mod configuration;
mod discovery;
mod settings;
mod steam_catalog;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use discovery::Snapshot;
use serde::Deserialize;
use serde_json::{Value, json};
use settings::{Exclusion, ProductSettings};
use std::collections::BTreeSet;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use steam_catalog::{Progress as SteamProgress, SteamCatalogRead};

pub const SERVICE: &str = "amaterasu.lutris/1";
const MANAGEMENT_CALLER: &str = "amaterasu.management";
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
        autosync_remove_uninstalled: Option<bool>,
        #[serde(default)]
        include_steam: Option<bool>,
        #[serde(default)]
        exclude_games: Option<Vec<Exclusion>>,
        expected_revision: u64,
    },
}

impl Action {
    fn is_settings(&self) -> bool {
        matches!(self, Self::SettingsGet {} | Self::SettingsUpdate { .. })
    }
}

struct SettingsPatch {
    enabled: bool,
    auto_sync: Option<bool>,
    autosync_remove_uninstalled: Option<bool>,
    include_steam: Option<bool>,
    exclude_games: Option<Vec<Exclusion>>,
}

impl SettingsPatch {
    fn apply(self, current: &ProductSettings) -> ProductSettings {
        ProductSettings {
            enabled: self.enabled,
            auto_sync: self.auto_sync.unwrap_or(current.auto_sync),
            autosync_remove_uninstalled: self
                .autosync_remove_uninstalled
                .unwrap_or(current.autosync_remove_uninstalled),
            include_steam: self.include_steam.unwrap_or(current.include_steam),
            exclude_games: self
                .exclude_games
                .unwrap_or_else(|| current.exclude_games.clone()),
        }
    }
}

#[derive(Default)]
struct SnapshotWorker {
    receiver: Option<Receiver<Result<Snapshot, String>>>,
    _thread: Option<JoinHandle<()>>,
    settings: Option<ProductSettings>,
}

impl SnapshotWorker {
    fn start(settings: ProductSettings) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let _ = sender.send(discovery::discover());
        });
        Self {
            receiver: Some(receiver),
            _thread: Some(worker),
            settings: Some(settings),
        }
    }

    fn take_result(&mut self) -> Option<(ProductSettings, Result<Snapshot, String>)> {
        let result = match self.receiver.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("Lutris scanner stopped before returning a snapshot".into())
            }
        };
        self.receiver = None;
        self._thread = None;
        let settings = self
            .settings
            .take()
            .expect("an active Lutris scan retains its captured settings");
        Some((settings, result))
    }

    fn busy(&self) -> bool {
        self.receiver.is_some()
    }
}

struct CapturedScan {
    snapshot: Snapshot,
    settings: ProductSettings,
    source_revision: u64,
}

impl CapturedScan {
    fn start_catalog(
        self,
        catalog: &mut catalog_sync::CatalogSync,
        steam_app_ids: &BTreeSet<u32>,
    ) -> Result<usize, String> {
        catalog.start(
            &self.snapshot,
            &self.settings,
            steam_app_ids,
            self.source_revision,
        )
    }
}

struct PendingSteamRead {
    scan: CapturedScan,
    reader: SteamCatalogRead,
}

struct LutrisModule {
    runtime: RuntimeClient,
    configuration: configuration::ProductConfiguration,
    catalog: catalog_sync::CatalogSync,
    snapshot_worker: SnapshotWorker,
    steam_read: Option<PendingSteamRead>,
    settings_epoch: u64,
    observed_settings_epoch: u64,
    next_auto_sync: Option<Instant>,
    manual_refresh_pending: bool,
    next_source_revision: u64,
    last_snapshot_games: usize,
    last_error: Option<String>,
}

impl ManagedModule for LutrisModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            configuration: configuration::ProductConfiguration::new(),
            catalog: catalog_sync::CatalogSync::default(),
            snapshot_worker: SnapshotWorker::default(),
            steam_read: None,
            settings_epoch: 0,
            observed_settings_epoch: u64::MAX,
            next_auto_sync: None,
            manual_refresh_pending: false,
            next_source_revision: 0,
            last_snapshot_games: 0,
            last_error: None,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.configuration
            .advance(&self.runtime, &mut self.settings_epoch)?;
        self.observe_settings();
        self.poll_snapshot();
        self.advance_steam_read()?;
        self.catalog.advance(&self.runtime)?;
        self.start_scan_if_due();
        match self.runtime.send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                payload,
                ..
            } => self.handle_request(id, &caller, payload),
            response => Err(format!("Lutris polling failed: {response:?}")),
        }
    }
}

impl LutrisModule {
    fn observe_settings(&mut self) {
        if !self.configuration.ready() || self.observed_settings_epoch == self.settings_epoch {
            return;
        }
        self.observed_settings_epoch = self.settings_epoch;
        let settings = self.configuration.settings();
        self.next_auto_sync = if settings.enabled && settings.auto_sync {
            Some(Instant::now())
        } else {
            None
        };
    }

    fn poll_snapshot(&mut self) {
        let Some((settings, result)) = self.snapshot_worker.take_result() else {
            return;
        };
        match result {
            Ok(snapshot) => {
                self.last_snapshot_games = snapshot.games.len();
                let Some(source_revision) = self.next_source_revision.checked_add(1) else {
                    self.last_error = Some("Lutris source revision exhausted".into());
                    return;
                };
                self.next_source_revision = source_revision;
                let scan = CapturedScan {
                    snapshot,
                    settings,
                    source_revision,
                };
                if scan.settings.include_steam {
                    self.steam_read = Some(PendingSteamRead {
                        scan,
                        reader: SteamCatalogRead::new(),
                    });
                } else {
                    self.start_catalog_scan(scan, &Default::default());
                }
            }
            Err(error) => self.last_error = Some(error),
        }
    }

    fn advance_steam_read(&mut self) -> ModuleResult<()> {
        let Some(mut pending) = self.steam_read.take() else {
            return Ok(());
        };
        match pending.reader.advance(&self.runtime)? {
            SteamProgress::Pending => self.steam_read = Some(pending),
            SteamProgress::Failed(error) => {
                self.last_error = Some(format!("Steam dedupe snapshot unavailable: {error}"));
            }
            SteamProgress::Complete(steam_ids) => {
                self.start_catalog_scan(pending.scan, &steam_ids);
            }
        }
        Ok(())
    }

    fn start_catalog_scan(&mut self, scan: CapturedScan, steam_app_ids: &BTreeSet<u32>) {
        if let Err(error) = scan.start_catalog(&mut self.catalog, steam_app_ids) {
            self.last_error = Some(error);
        } else {
            self.last_error = None;
        }
    }

    fn start_scan_if_due(&mut self) {
        if !self.configuration.ready()
            || self.snapshot_worker.busy()
            || self.steam_read.is_some()
            || self.catalog.busy()
        {
            return;
        }
        let settings = self.configuration.settings();
        if !settings.enabled {
            self.manual_refresh_pending = false;
            self.next_auto_sync = None;
            return;
        }
        if !scan_is_due(
            settings,
            self.manual_refresh_pending,
            self.next_auto_sync,
            Instant::now(),
        ) {
            return;
        }
        self.manual_refresh_pending = false;
        self.next_auto_sync = if settings.auto_sync {
            Some(Instant::now() + AUTO_SYNC_INTERVAL)
        } else {
            None
        };
        self.snapshot_worker = SnapshotWorker::start(settings.clone());
    }

    fn handle_request(&mut self, request: u64, caller: &str, payload: Value) -> ModuleResult<()> {
        let action = match serde_json::from_value::<Action>(payload) {
            Ok(action) => action,
            Err(_) => return self.complete(request, json!({"ok":false,"code":"invalid_request"})),
        };
        if !is_management_caller(caller) {
            return self.complete(request, json!({"ok":false,"code":"forbidden"}));
        }
        if action.is_settings() && self.configuration.busy() {
            let code = if self.configuration.ready() {
                "settings_busy"
            } else {
                "settings_loading"
            };
            return self.complete(request, json!({"ok":false,"code":code}));
        }
        if !self.configuration.ready() && matches!(action, Action::Refresh {}) {
            return self.complete(request, json!({"ok":false,"code":"settings_loading"}));
        }
        match action {
            Action::Status {} => self.complete(request, self.status()),
            Action::Refresh {} => {
                if !self.configuration.settings().enabled {
                    return self.complete(request, json!({"ok":false,"code":"lutris_disabled"}));
                }
                self.manual_refresh_pending = true;
                self.complete(request, self.status())
            }
            Action::SettingsGet {} => {
                self.complete(request, self.configuration.management_response())
            }
            Action::SettingsUpdate {
                enabled,
                auto_sync,
                autosync_remove_uninstalled,
                include_steam,
                exclude_games,
                expected_revision,
            } => {
                let patch = SettingsPatch {
                    enabled,
                    auto_sync,
                    autosync_remove_uninstalled,
                    include_steam,
                    exclude_games,
                };
                let settings = patch.apply(self.configuration.settings());
                match self
                    .configuration
                    .begin_update(request, settings, expected_revision)
                {
                    Ok(()) => Ok(()),
                    Err(response) => self.complete(request, response),
                }
            }
        }
    }

    fn status(&self) -> Value {
        let available = discovery::availability();
        json!({
            "ok":true,
            "settings_ready":self.configuration.ready(),
            "enabled":self.configuration.settings().enabled,
            "auto_sync":self.configuration.settings().auto_sync,
            "autosync_remove_uninstalled":self.configuration.settings().autosync_remove_uninstalled,
            "include_steam":self.configuration.settings().include_steam,
            "available":available.supported_platform && available.database_available && available.executable_available,
            "platform_supported":available.supported_platform,
            "database_available":available.database_available,
            "executable_available":available.executable_available,
            "settings_revision":self.configuration.revision(),
            "game_count":self.last_snapshot_games,
            "refresh_pending":self.manual_refresh_pending || self.snapshot_worker.busy() || self.steam_read.is_some(),
            "last_error":self.last_error,
            "catalog_sync":self.catalog.status(),
        })
    }

    fn complete(&self, request: u64, payload: Value) -> ModuleResult<()> {
        match self.runtime.send(Command::Complete { request, payload })? {
            Response::Ok => Ok(()),
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                Ok(())
            }
            response => Err(format!("Lutris request completion failed: {response:?}")),
        }
    }
}

fn scan_is_due(
    settings: &ProductSettings,
    manual_refresh_pending: bool,
    next_auto_sync: Option<Instant>,
    now: Instant,
) -> bool {
    settings.enabled
        && (manual_refresh_pending
            || (settings.auto_sync && next_auto_sync.is_some_and(|deadline| now >= deadline)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{Game, Snapshot};
    use crate::settings::Exclusion;
    use std::path::PathBuf;

    fn game(id: i64, runner: &str, service: &str, service_id: &str) -> Game {
        Game {
            id,
            name: format!("Lutris Game {id}"),
            slug: format!("lutris-game-{id}"),
            runner: runner.into(),
            platform: "linux".into(),
            directory: "/games/example".into(),
            config_path: "/games/example.yml".into(),
            service: service.into(),
            service_id: service_id.into(),
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            database_path: PathBuf::from("/home/user/.local/share/lutris/pga.db"),
            lutris_executable: if cfg!(target_os = "windows") {
                std::env::temp_dir().join("lutris.exe")
            } else {
                PathBuf::from("/usr/bin/lutris")
            },
            games: vec![
                game(7, "steam", "steam", "9001"),
                game(8, "steam", "steam", "9002"),
                game(10, "wine", "", ""),
            ],
        }
    }

    fn completed_snapshot_worker(settings: ProductSettings, snapshot: Snapshot) -> SnapshotWorker {
        let (sender, receiver) = mpsc::sync_channel(1);
        let _ = sender.send(Ok(snapshot));
        SnapshotWorker {
            receiver: Some(receiver),
            _thread: None,
            settings: Some(settings),
        }
    }

    #[test]
    fn settings_updates_are_flat_patch_compatible_and_caller_bound() {
        let action = serde_json::from_value::<Action>(json!({
            "action":"settings_update",
            "enabled":true,
            "expected_revision":3,
            "include_steam":true
        }))
        .unwrap();
        assert!(action.is_settings());
        assert!(
            serde_json::from_value::<Action>(json!({
                "action":"settings_update",
                "enabled":true,
                "expected_revision":3,
                "owner":"amaterasu.management"
            }))
            .is_err()
        );
        assert!(is_management_caller(MANAGEMENT_CALLER));
        assert!(!is_management_caller("amaterasu.steam"));
    }

    #[test]
    fn older_settings_update_preserves_newer_optional_values() {
        let current = ProductSettings {
            enabled: true,
            auto_sync: false,
            include_steam: true,
            autosync_remove_uninstalled: false,
            exclude_games: vec![Exclusion::Id("17".into())],
        };
        let patch = SettingsPatch {
            enabled: false,
            auto_sync: None,
            autosync_remove_uninstalled: None,
            include_steam: None,
            exclude_games: None,
        };
        let updated = patch.apply(&current);
        assert!(!updated.enabled);
        assert!(!updated.auto_sync);
        assert!(!updated.autosync_remove_uninstalled);
        assert!(updated.include_steam);
        assert_eq!(updated.exclude_games, current.exclude_games);
    }

    #[test]
    fn auto_sync_interval_matches_pinned_host_policy() {
        assert_eq!(AUTO_SYNC_INTERVAL, Duration::from_secs(30));
    }

    #[test]
    fn disable_during_scan_keeps_captured_settings_through_steam_dedupe() {
        let scan_start_settings = ProductSettings {
            enabled: true,
            auto_sync: false,
            autosync_remove_uninstalled: false,
            include_steam: true,
            exclude_games: Vec::new(),
        };
        let mut worker = completed_snapshot_worker(scan_start_settings.clone(), snapshot());

        let disabled_settings = SettingsPatch {
            enabled: false,
            auto_sync: Some(true),
            autosync_remove_uninstalled: Some(true),
            include_steam: Some(false),
            exclude_games: Some(vec![Exclusion::Id("10".into())]),
        }
        .apply(&scan_start_settings);
        assert!(!disabled_settings.enabled);
        assert!(disabled_settings.autosync_remove_uninstalled);
        assert!(!disabled_settings.include_steam);
        assert_eq!(
            disabled_settings.exclude_games,
            vec![Exclusion::Id("10".into())]
        );
        assert!(worker.busy());

        let (captured_settings, result) = worker.take_result().unwrap();
        assert!(!worker.busy());
        let scan = CapturedScan {
            snapshot: result.unwrap(),
            settings: captured_settings,
            source_revision: 1,
        };
        assert_eq!(scan.settings, scan_start_settings);
        assert!(scan.settings.include_steam);

        let pending = PendingSteamRead {
            scan,
            reader: SteamCatalogRead::new(),
        };
        assert_eq!(pending.scan.settings, scan_start_settings);
        assert!(!disabled_settings.include_steam);

        let mut catalog = catalog_sync::CatalogSync::default();
        let count = pending
            .scan
            .start_catalog(&mut catalog, &[9001].into())
            .unwrap();
        assert_eq!(count, 2);
        let transaction = catalog.transaction.as_ref().unwrap();
        let submitted_entries = transaction
            .batches
            .iter()
            .flat_map(|batch| {
                batch
                    .entries
                    .iter()
                    .filter_map(|entry| entry["source_id"].as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(submitted_entries, ["8", "10"]);
        let selection_removals = transaction
            .batches
            .iter()
            .flat_map(|batch| batch.selection_remove_source_ids.iter().map(String::as_str))
            .collect::<Vec<_>>();
        assert_eq!(selection_removals, ["7"]);
        assert!(!selection_removals.contains(&"10"));
        assert_eq!(transaction.payload()["action"], "provider_snapshot_begin");

        let now = Instant::now();
        assert!(!scan_is_due(
            &disabled_settings,
            true,
            Some(now - Duration::from_secs(1)),
            now,
        ));
    }
}

fn is_management_caller(caller: &str) -> bool {
    caller == MANAGEMENT_CALLER
}

amaterasu_module_sdk::export_module!(LutrisModule);
