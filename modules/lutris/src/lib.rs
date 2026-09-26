//! Linux Lutris discovery, product settings, and catalog import provider.

mod artwork;
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
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use steam_catalog::{Progress as SteamProgress, SteamCatalogRead};

pub const SERVICE: &str = "amaterasu.lutris/1";
const MANAGEMENT_CALLER: &str = "amaterasu.management";
const AUTO_SYNC_INTERVAL: Duration = Duration::from_secs(30);
const RECONCILIATION_RETRY_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceSignature {
    executable: std::path::PathBuf,
    launch_kind: discovery::LaunchKind,
    games: Vec<discovery::Game>,
    artwork_revisions: std::collections::BTreeMap<i64, String>,
    artwork_scan_incomplete: bool,
}

impl From<&Snapshot> for SourceSignature {
    fn from(snapshot: &Snapshot) -> Self {
        Self {
            executable: snapshot.lutris_executable.clone(),
            launch_kind: snapshot.launch_kind,
            games: snapshot.games.clone(),
            artwork_revisions: snapshot.artwork_revisions.clone(),
            artwork_scan_incomplete: snapshot.artwork_scan_incomplete,
        }
    }
}

fn should_reconcile(
    signature: &SourceSignature,
    previous: Option<&SourceSignature>,
    previous_at: Option<Instant>,
    manual: bool,
    now: Instant,
) -> bool {
    manual
        || previous != Some(signature)
        || previous_at.is_none_or(|at| now.duration_since(at) >= RECONCILIATION_RETRY_INTERVAL)
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Status {},
    Refresh {},
    ArtworkGet {
        game_id: i64,
    },
    ArtworkRead {
        game_id: i64,
        revision: String,
        offset: u64,
    },
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
    manual: bool,
    settings_epoch: u64,
}

impl SnapshotWorker {
    fn start(settings: ProductSettings, manual: bool, settings_epoch: u64) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let _ = sender.send(discovery::discover());
        });
        Self {
            receiver: Some(receiver),
            _thread: Some(worker),
            settings: Some(settings),
            manual,
            settings_epoch,
        }
    }

    fn take_result(&mut self) -> Option<(ProductSettings, bool, u64, Result<Snapshot, String>)> {
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
        Some((settings, self.manual, self.settings_epoch, result))
    }

    fn busy(&self) -> bool {
        self.receiver.is_some()
    }
}

struct CapturedScan {
    snapshot: Snapshot,
    settings: ProductSettings,
    source_revision: u64,
    settings_epoch: u64,
}

impl CapturedScan {
    fn start_catalog(
        self,
        catalog: &mut catalog_sync::CatalogSync,
        steam_app_ids: &BTreeSet<u32>,
        current_epoch: u64,
        enabled: bool,
    ) -> Result<Option<(usize, SourceSignature, u64, Snapshot)>, String> {
        if !enabled || self.settings_epoch != current_epoch {
            return Ok(None);
        }
        let count = catalog.start(
            &self.snapshot,
            &self.settings,
            steam_app_ids,
            self.source_revision,
        )?;
        Ok(Some((
            count,
            SourceSignature::from(&self.snapshot),
            self.settings_epoch,
            self.snapshot,
        )))
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
    artwork: artwork::ArtworkStore,
    snapshot_worker: SnapshotWorker,
    steam_read: Option<PendingSteamRead>,
    settings_epoch: u64,
    observed_settings_epoch: u64,
    next_auto_sync: Option<Instant>,
    manual_refresh_pending: bool,
    next_source_revision: u64,
    last_snapshot_games: usize,
    last_artwork_scan_incomplete: bool,
    last_artwork_revisions: BTreeMap<i64, String>,
    last_error: Option<String>,
    last_reconciled_source: Option<SourceSignature>,
    last_reconciled_at: Option<Instant>,
    pending_source: Option<(SourceSignature, u64)>,
    pending_artwork: Option<(Snapshot, u64)>,
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
            artwork: artwork::ArtworkStore::default(),
            snapshot_worker: SnapshotWorker::default(),
            steam_read: None,
            settings_epoch: 0,
            observed_settings_epoch: u64::MAX,
            next_auto_sync: None,
            manual_refresh_pending: false,
            next_source_revision: 0,
            last_snapshot_games: 0,
            last_artwork_scan_incomplete: false,
            last_artwork_revisions: BTreeMap::new(),
            last_error: None,
            last_reconciled_source: None,
            last_reconciled_at: None,
            pending_source: None,
            pending_artwork: None,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.configuration
            .advance(&self.runtime, &mut self.settings_epoch)?;
        self.observe_settings();
        self.poll_snapshot();
        self.advance_steam_read()?;
        self.catalog.advance(&self.runtime)?;
        self.finish_catalog_if_ready();
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
        self.artwork.clear();
        self.last_artwork_revisions.clear();
        self.last_reconciled_source = None;
        self.last_reconciled_at = None;
        let settings = self.configuration.settings();
        schedule_settings_scan(
            settings,
            &mut self.manual_refresh_pending,
            &mut self.next_auto_sync,
            Instant::now(),
        );
    }

    fn poll_snapshot(&mut self) {
        let Some((settings, manual, settings_epoch, result)) = self.snapshot_worker.take_result()
        else {
            return;
        };
        if settings_epoch != self.settings_epoch || !self.configuration.settings().enabled {
            self.reschedule_stale_scan(settings_epoch);
            return;
        }
        match result {
            Ok(mut snapshot) => {
                preserve_unscanned_artwork(&mut snapshot, &self.last_artwork_revisions);
                self.last_snapshot_games = snapshot.games.len();
                self.last_artwork_scan_incomplete = snapshot.artwork_scan_incomplete;
                let signature = SourceSignature::from(&snapshot);
                if !should_reconcile(
                    &signature,
                    self.last_reconciled_source.as_ref(),
                    self.last_reconciled_at,
                    manual,
                    Instant::now(),
                ) {
                    self.schedule_next_auto_sync(settings_epoch != self.settings_epoch);
                    return;
                }
                let Some(source_revision) = self.next_source_revision.checked_add(1) else {
                    self.last_error = Some("Lutris source revision exhausted".into());
                    self.schedule_next_auto_sync(settings_epoch != self.settings_epoch);
                    return;
                };
                self.next_source_revision = source_revision;
                let scan = CapturedScan {
                    snapshot,
                    settings,
                    source_revision,
                    settings_epoch,
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
            Err(error) => {
                self.last_error = Some(error);
                self.schedule_next_auto_sync(settings_epoch != self.settings_epoch);
            }
        }
    }

    fn advance_steam_read(&mut self) -> ModuleResult<()> {
        let Some(mut pending) = self.steam_read.take() else {
            return Ok(());
        };
        let progress = pending.reader.advance(&self.runtime)?;
        match progress {
            SteamProgress::Pending => self.steam_read = Some(pending),
            SteamProgress::Failed(error) => {
                if record_steam_failure_if_current(
                    pending.scan.settings_epoch,
                    self.settings_epoch,
                    self.configuration.settings().enabled,
                    &error,
                    &mut self.last_error,
                    &mut self.last_reconciled_source,
                    &mut self.last_reconciled_at,
                ) {
                    self.pending_source = None;
                    self.schedule_next_auto_sync(false);
                } else {
                    self.reschedule_stale_scan(pending.scan.settings_epoch);
                }
            }
            SteamProgress::Complete(steam_ids) => {
                self.start_catalog_scan(pending.scan, &steam_ids);
            }
        }
        Ok(())
    }

    fn start_catalog_scan(&mut self, scan: CapturedScan, steam_app_ids: &BTreeSet<u32>) {
        let settings_changed = scan.settings_epoch != self.settings_epoch;
        let captured_epoch = scan.settings_epoch;
        match scan.start_catalog(
            &mut self.catalog,
            steam_app_ids,
            self.settings_epoch,
            self.configuration.settings().enabled,
        ) {
            Ok(Some((_, signature, settings_epoch, snapshot))) => {
                self.pending_source = Some((signature, settings_epoch));
                self.pending_artwork = Some((snapshot, settings_epoch));
                self.last_error = None;
            }
            Ok(None) => self.reschedule_stale_scan(captured_epoch),
            Err(error) => {
                self.last_error = Some(error);
                self.last_reconciled_source = None;
                self.last_reconciled_at = None;
                self.schedule_next_auto_sync(settings_changed);
            }
        }
    }

    fn finish_catalog_if_ready(&mut self) {
        let Some(settings_changed) = finish_reconciliation(
            &mut self.pending_source,
            &self.catalog,
            self.settings_epoch,
            &mut self.last_reconciled_source,
            &mut self.last_reconciled_at,
            &mut self.last_error,
        ) else {
            return;
        };
        publish_reconciled_artwork(
            &mut self.pending_artwork,
            &self.catalog,
            &mut self.artwork,
            &mut self.last_artwork_revisions,
            self.settings_epoch,
            self.configuration.settings().enabled,
        );
        self.schedule_next_auto_sync(settings_changed);
    }

    fn schedule_next_auto_sync(&mut self, settings_changed: bool) {
        self.next_auto_sync = next_auto_sync_after_scan(
            self.configuration.settings(),
            settings_changed,
            Instant::now(),
        );
    }

    fn reschedule_stale_scan(&mut self, captured_epoch: u64) {
        reschedule_stale_scan(
            self.configuration.settings(),
            captured_epoch != self.settings_epoch,
            &mut self.manual_refresh_pending,
            &mut self.next_auto_sync,
            Instant::now(),
        );
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
        let manual = self.manual_refresh_pending;
        self.manual_refresh_pending = false;
        self.next_auto_sync = None;
        self.snapshot_worker = SnapshotWorker::start(settings.clone(), manual, self.settings_epoch);
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
            Action::ArtworkGet { game_id } => self.complete(
                request,
                artwork_response(
                    &self.artwork,
                    self.configuration.settings().enabled,
                    caller,
                    game_id,
                    None,
                ),
            ),
            Action::ArtworkRead {
                game_id,
                revision,
                offset,
            } => self.complete(
                request,
                artwork_response(
                    &self.artwork,
                    self.configuration.settings().enabled,
                    caller,
                    game_id,
                    Some((&revision, offset)),
                ),
            ),
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
            "available":available.paired_source_available,
            "platform_supported":available.supported_platform,
            "database_available":available.database_available,
            "executable_available":available.executable_available,
            "settings_revision":self.configuration.revision(),
            "game_count":self.last_snapshot_games,
            "artwork_scan_incomplete":self.last_artwork_scan_incomplete,
            "refresh_pending":self.manual_refresh_pending || self.snapshot_worker.busy() || self.steam_read.is_some() || self.catalog.busy(),
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

fn artwork_response(
    store: &artwork::ArtworkStore,
    enabled: bool,
    caller: &str,
    game_id: i64,
    read: Option<(&str, u64)>,
) -> Value {
    if !is_management_caller(caller) {
        return json!({"ok":false,"code":"forbidden"});
    }
    if !enabled {
        return json!({"ok":false,"code":"lutris_disabled"});
    }
    let result = match read {
        Some((revision, offset)) => store.read(game_id, revision, offset),
        None => store.metadata(game_id),
    };
    result.unwrap_or_else(|code| json!({"ok":false,"code":code}))
}

pub(crate) fn preserve_unscanned_artwork(
    snapshot: &mut Snapshot,
    previous: &BTreeMap<i64, String>,
) {
    if !snapshot.artwork_scan_incomplete {
        return;
    }
    for game in snapshot.games.iter().skip(snapshot.artwork_scanned_games) {
        if let Some(revision) = previous.get(&game.id) {
            snapshot.artwork_revisions.insert(game.id, revision.clone());
        }
    }
}

fn retain_current_artwork(
    snapshot: &mut Snapshot,
    previous: &mut BTreeMap<i64, String>,
    captured_epoch: u64,
    current_epoch: u64,
    enabled: bool,
) {
    if !enabled || captured_epoch != current_epoch {
        return;
    }
    preserve_unscanned_artwork(snapshot, previous);
    *previous = snapshot.artwork_revisions.clone();
}

fn publish_reconciled_artwork(
    pending: &mut Option<(Snapshot, u64)>,
    catalog: &catalog_sync::CatalogSync,
    artwork: &mut artwork::ArtworkStore,
    previous: &mut BTreeMap<i64, String>,
    current_epoch: u64,
    enabled: bool,
) {
    if catalog.busy() {
        return;
    }
    let Some((snapshot, captured_epoch)) = pending.take() else {
        return;
    };
    if enabled && captured_epoch == current_epoch && catalog.succeeded() {
        artwork.update_if_current(&snapshot, captured_epoch, current_epoch, enabled);
        let mut snapshot = snapshot;
        retain_current_artwork(
            &mut snapshot,
            previous,
            captured_epoch,
            current_epoch,
            enabled,
        );
    }
}

fn next_auto_sync_after_scan(
    settings: &ProductSettings,
    settings_changed: bool,
    now: Instant,
) -> Option<Instant> {
    (settings.enabled && settings.auto_sync).then(|| {
        now + if settings_changed {
            Duration::ZERO
        } else {
            AUTO_SYNC_INTERVAL
        }
    })
}

fn schedule_settings_scan(
    settings: &ProductSettings,
    scan_pending: &mut bool,
    next_auto_sync: &mut Option<Instant>,
    now: Instant,
) {
    *scan_pending = settings.enabled;
    *next_auto_sync = (settings.enabled && settings.auto_sync).then_some(now);
}

fn reschedule_stale_scan(
    settings: &ProductSettings,
    settings_changed: bool,
    manual_refresh_pending: &mut bool,
    next_auto_sync: &mut Option<Instant>,
    now: Instant,
) {
    if settings.enabled {
        *manual_refresh_pending = true;
    }
    *next_auto_sync = next_auto_sync_after_scan(settings, settings_changed, now);
}

fn record_steam_failure_if_current(
    captured_epoch: u64,
    current_epoch: u64,
    enabled: bool,
    error: &str,
    last_error: &mut Option<String>,
    last_reconciled_source: &mut Option<SourceSignature>,
    last_reconciled_at: &mut Option<Instant>,
) -> bool {
    if !enabled || captured_epoch != current_epoch {
        return false;
    }
    *last_error = Some(format!("Steam dedupe snapshot unavailable: {error}"));
    *last_reconciled_source = None;
    *last_reconciled_at = None;
    true
}

fn finish_reconciliation(
    pending_source: &mut Option<(SourceSignature, u64)>,
    catalog: &catalog_sync::CatalogSync,
    settings_epoch: u64,
    last_reconciled_source: &mut Option<SourceSignature>,
    last_reconciled_at: &mut Option<Instant>,
    last_error: &mut Option<String>,
) -> Option<bool> {
    if catalog.busy() {
        return None;
    }
    let (signature, captured_settings_epoch) = pending_source.take()?;
    let settings_changed = captured_settings_epoch != settings_epoch;
    if catalog.succeeded() && !settings_changed {
        *last_reconciled_source = Some(signature);
        *last_reconciled_at = Some(Instant::now());
        *last_error = None;
    } else {
        *last_reconciled_source = None;
        *last_reconciled_at = None;
        if let Some(error) = catalog.last_error() {
            *last_error = Some(format!("Lutris catalog reconciliation failed: {error}"));
        }
    }
    Some(settings_changed)
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
    #[cfg(target_os = "linux")]
    use std::fs::File;
    use std::path::PathBuf;
    #[cfg(target_os = "linux")]
    use std::time::{SystemTime, UNIX_EPOCH};

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
            launch_kind: discovery::LaunchKind::Native,
            games: vec![
                game(7, "steam", "steam", "9001"),
                game(8, "steam", "steam", "9002"),
                game(10, "wine", "", ""),
            ],
            artwork_revisions: Default::default(),
            artwork_scan_incomplete: false,
            artwork_scanned_games: 3,
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn artwork_actions_read_bounded_current_images_without_exposing_paths() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("amaterasu-lutris-artwork-{nonce}"));
        let lutris_data = root.join("lutris");
        let cover = lutris_data.join("coverart/lutris-game-10.png");
        std::fs::create_dir_all(cover.parent().unwrap()).unwrap();
        let first = [b"\x89PNG\r\n\x1a\n".as_slice(), b"first"].concat();
        std::fs::write(&cover, &first).unwrap();
        let mut source = snapshot();
        source.database_path = lutris_data.join("pga.db");
        let mut store = artwork::ArtworkStore::default();
        store.update(&source);

        let metadata = artwork_response(&store, true, MANAGEMENT_CALLER, 10, None);
        let revision = metadata["revision"].as_str().unwrap().to_owned();
        assert_eq!(metadata["mime_type"], "image/png");
        assert_eq!(metadata["total_bytes"], first.len());
        assert!(metadata.get("path").is_none());
        let chunk = artwork_response(&store, true, MANAGEMENT_CALLER, 10, Some((&revision, 0)));
        assert_eq!(chunk["ok"], true);
        assert_eq!(chunk["next_offset"], Value::Null);
        assert!(chunk.get("path").is_none());
        assert_eq!(
            artwork_response(&store, true, "amaterasu.other", 10, None)["code"],
            "forbidden"
        );
        assert_eq!(
            artwork_response(&store, false, MANAGEMENT_CALLER, 10, None)["code"],
            "lutris_disabled"
        );
        assert_eq!(
            artwork_response(&store, true, MANAGEMENT_CALLER, 10, Some((&revision, 1)))["code"],
            "invalid_cursor"
        );

        let replacement_path = cover.with_extension("replacement");
        std::fs::write(
            &replacement_path,
            [b"\x89PNG\r\n\x1a\n".as_slice(), b"other"].concat(),
        )
        .unwrap();
        std::fs::rename(&replacement_path, &cover).unwrap();
        let same_size_revision = artwork_response(&store, true, MANAGEMENT_CALLER, 10, None);
        assert_ne!(same_size_revision["revision"], revision);

        std::fs::write(
            &cover,
            [b"\x89PNG\r\n\x1a\n".as_slice(), b"replacement"].concat(),
        )
        .unwrap();
        let changed = artwork_response(&store, true, MANAGEMENT_CALLER, 10, None);
        assert_ne!(changed["revision"], revision);
        assert_eq!(
            artwork_response(&store, true, MANAGEMENT_CALLER, 10, Some((&revision, 0)))["code"],
            "artwork_changed"
        );
        File::create(&cover)
            .unwrap()
            .set_len(16 * 1024 * 1024 + 1)
            .unwrap();
        let icon = root.join("icons/hicolor/256x256/apps/lutris_lutris-game-10.png");
        std::fs::create_dir_all(icon.parent().unwrap()).unwrap();
        std::fs::write(&icon, &first).unwrap();
        assert_eq!(
            artwork_response(&store, true, MANAGEMENT_CALLER, 10, None)["mime_type"],
            "image/png"
        );
        std::fs::remove_file(icon).unwrap();
        assert_eq!(
            artwork_response(&store, true, MANAGEMENT_CALLER, 10, None)["code"],
            "artwork_unavailable"
        );

        source.games[2].slug = "../outside".into();
        store.update(&source);
        assert_eq!(
            artwork_response(&store, true, MANAGEMENT_CALLER, 10, None)["code"],
            "artwork_unavailable"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn artwork_sources_require_a_current_enabled_scan_after_disable() {
        let source = snapshot();
        let mut store = artwork::ArtworkStore::default();
        store.update_if_current(&source, 3, 3, true);
        assert!(store.contains_game_for_test(10));

        // A disable clears the source, and a late scan captured before it cannot republish it.
        store.clear();
        store.update_if_current(&source, 3, 4, false);
        assert!(!store.contains_game_for_test(10));

        // Re-enable with auto-sync off still requires a new manual discovery.
        store.clear();
        store.update_if_current(&source, 3, 5, true);
        assert!(!store.contains_game_for_test(10));
        store.update_if_current(&source, 5, 5, true);
        assert!(store.contains_game_for_test(10));
    }

    #[test]
    fn late_old_epoch_artwork_cannot_seed_a_partial_reenable_scan() {
        let mut previous = BTreeMap::from([(10, "a".repeat(64))]);
        previous.clear(); // disabling invalidates the remembered source
        let mut late = snapshot();
        late.artwork_scan_incomplete = true;
        late.artwork_scanned_games = 0;
        late.artwork_revisions.insert(10, "a".repeat(64));
        retain_current_artwork(&mut late, &mut previous, 3, 4, false);
        assert!(previous.is_empty());
        retain_current_artwork(&mut late, &mut previous, 3, 5, true);
        assert!(previous.is_empty());

        let mut current_partial = snapshot();
        current_partial.artwork_scan_incomplete = true;
        current_partial.artwork_scanned_games = 0;
        retain_current_artwork(&mut current_partial, &mut previous, 5, 5, true);
        assert!(!current_partial.artwork_revisions.contains_key(&10));
        assert!(previous.is_empty());
    }

    fn completed_snapshot_worker(settings: ProductSettings, snapshot: Snapshot) -> SnapshotWorker {
        let (sender, receiver) = mpsc::sync_channel(1);
        let _ = sender.send(Ok(snapshot));
        SnapshotWorker {
            receiver: Some(receiver),
            _thread: None,
            settings: Some(settings),
            manual: false,
            settings_epoch: 3,
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
    fn reenable_after_in_flight_discovery_failure_scans_immediately() {
        let settings_at_start = ProductSettings {
            enabled: true,
            auto_sync: true,
            ..ProductSettings::default()
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        sender
            .send(Err("lutris_database_unreadable".into()))
            .unwrap();
        let mut worker = SnapshotWorker {
            receiver: Some(receiver),
            _thread: None,
            settings: Some(settings_at_start.clone()),
            manual: false,
            settings_epoch: 3,
        };

        let mut disabled = settings_at_start.clone();
        disabled.enabled = false;
        let now = Instant::now();
        assert_eq!(next_auto_sync_after_scan(&disabled, true, now), None);
        assert!(!scan_is_due(&disabled, false, Some(now), now));

        // The old scan fails after disable and re-enable have both been persisted.
        let (captured, manual, captured_epoch, result) = worker.take_result().unwrap();
        assert_eq!(captured, settings_at_start);
        assert!(!manual);
        assert_eq!(captured_epoch, 3);
        assert_eq!(result, Err("lutris_database_unreadable".into()));
        let current_epoch = 5;
        let deadline =
            next_auto_sync_after_scan(&settings_at_start, captured_epoch != current_epoch, now);
        assert_eq!(deadline, Some(now));
        assert!(scan_is_due(&settings_at_start, false, deadline, now));
        assert_eq!(
            next_auto_sync_after_scan(&settings_at_start, false, now),
            Some(now + AUTO_SYNC_INTERVAL)
        );

        let mut manual_only = settings_at_start;
        manual_only.auto_sync = false;
        assert_eq!(next_auto_sync_after_scan(&manual_only, true, now), None);
    }

    #[test]
    fn stale_automatic_scan_and_reenable_each_trigger_manual_only_discovery() {
        let settings = ProductSettings {
            enabled: true,
            auto_sync: false,
            ..ProductSettings::default()
        };
        let now = Instant::now();
        let mut manual_pending = false;
        let mut next_auto_sync = None;
        reschedule_stale_scan(
            &settings,
            true,
            &mut manual_pending,
            &mut next_auto_sync,
            now,
        );
        assert!(manual_pending);
        assert_eq!(next_auto_sync, None);
        assert!(scan_is_due(&settings, manual_pending, next_auto_sync, now));

        manual_pending = false;
        let disabled = ProductSettings {
            enabled: false,
            ..settings.clone()
        };
        reschedule_stale_scan(
            &disabled,
            true,
            &mut manual_pending,
            &mut next_auto_sync,
            now,
        );
        assert!(!manual_pending);
        assert!(!scan_is_due(&disabled, manual_pending, next_auto_sync, now));

        schedule_settings_scan(&settings, &mut manual_pending, &mut next_auto_sync, now);
        assert!(manual_pending);
        assert_eq!(next_auto_sync, None);
        assert!(scan_is_due(&settings, manual_pending, next_auto_sync, now));
    }

    #[test]
    fn stale_steam_failure_preserves_current_reconciliation_state() {
        let signature = SourceSignature::from(&snapshot());
        let now = Instant::now();
        let mut last_error = Some("current scan error".to_owned());
        let mut last_source = Some(signature.clone());
        let mut last_at = Some(now);
        assert!(!record_steam_failure_if_current(
            3,
            5,
            true,
            "old catalog failure",
            &mut last_error,
            &mut last_source,
            &mut last_at,
        ));
        assert_eq!(last_error.as_deref(), Some("current scan error"));
        assert_eq!(last_source, Some(signature.clone()));
        assert_eq!(last_at, Some(now));

        assert!(record_steam_failure_if_current(
            5,
            5,
            true,
            "current catalog failure",
            &mut last_error,
            &mut last_source,
            &mut last_at,
        ));
        assert!(last_error.unwrap().contains("current catalog failure"));
        assert_eq!(last_source, None);
        assert_eq!(last_at, None);
    }

    #[test]
    fn unchanged_automatic_source_skips_catalog_until_retry_but_manual_refresh_runs() {
        let snapshot = snapshot();
        let signature = SourceSignature::from(&snapshot);
        let now = Instant::now();
        assert!(should_reconcile(&signature, None, None, false, now));
        assert!(!should_reconcile(
            &signature,
            Some(&signature),
            Some(now),
            false,
            now
        ));
        assert!(should_reconcile(
            &signature,
            Some(&signature),
            Some(now),
            true,
            now
        ));
        assert!(should_reconcile(
            &signature,
            Some(&signature),
            Some(now),
            false,
            now + RECONCILIATION_RETRY_INTERVAL,
        ));

        let mut changed = snapshot;
        changed.games[0].service_id = "changed".into();
        assert!(should_reconcile(
            &SourceSignature::from(&changed),
            Some(&signature),
            Some(now),
            false,
            now,
        ));
        changed.games[0].service_id = "9001".into();
        changed.games[0].config_path = "/new/config.yml".into();
        assert_ne!(SourceSignature::from(&changed), signature);
        changed.games[0].config_path = "/games/example.yml".into();
        changed.artwork_revisions.insert(10, "a".repeat(64));
        assert!(should_reconcile(
            &SourceSignature::from(&changed),
            Some(&signature),
            Some(now),
            false,
            now,
        ));
        changed.artwork_revisions.clear();
        changed.launch_kind = discovery::LaunchKind::FlatpakUser;
        assert!(should_reconcile(
            &SourceSignature::from(&changed),
            Some(&signature),
            Some(now),
            false,
            now,
        ));
    }

    #[test]
    fn steam_dedupe_does_not_record_source_before_catalog_finish() {
        let mut source = snapshot();
        source.games.clear();
        let settings = ProductSettings {
            include_steam: true,
            ..ProductSettings::default()
        };
        let pending = PendingSteamRead {
            scan: CapturedScan {
                snapshot: source.clone(),
                settings,
                source_revision: 1,
                settings_epoch: 7,
            },
            reader: SteamCatalogRead::new(),
        };
        let mut catalog = catalog_sync::CatalogSync::default();
        assert!(!catalog.busy());
        assert!(catalog.succeeded());

        // The receipt is created only when the deduped scan starts a catalog transaction.
        let (_, signature, epoch, _) = pending
            .scan
            .start_catalog(&mut catalog, &BTreeSet::new(), 7, true)
            .unwrap()
            .unwrap();
        assert_eq!(signature, SourceSignature::from(&source));
        assert_eq!(epoch, 7);
        assert!(catalog.busy());
        assert!(!catalog.succeeded());
        catalog.respond_for_test(json!({"ok":true,"revision":1,"generation":2,"next_sequence":0}));
        assert!(!catalog.succeeded());
        catalog.respond_for_test(json!({
            "ok":true,"revision":1,"generation":2,"complete":true,
            "cleanup_pending":false,"remove_stale":true,"remove_selection":true
        }));
        assert!(catalog.succeeded());

        let failing_scan = CapturedScan {
            snapshot: source,
            settings: ProductSettings {
                include_steam: true,
                ..ProductSettings::default()
            },
            source_revision: 2,
            settings_epoch: 7,
        };
        failing_scan
            .start_catalog(&mut catalog, &BTreeSet::new(), 7, true)
            .unwrap()
            .unwrap();
        catalog.respond_for_test(json!({"ok":true,"revision":2,"generation":3,"next_sequence":0}));
        catalog.respond_for_test(json!({
            "ok":true,"revision":2,"generation":3,"complete":false,
            "cleanup_pending":false,"remove_stale":true,"remove_selection":true
        }));
        assert!(!catalog.busy());
        assert!(!catalog.succeeded());
    }

    #[test]
    fn failed_reconciliation_is_visible_and_a_later_scan_recovers() {
        let mut source = snapshot();
        source.games.clear();
        let settings = ProductSettings::default();
        let signature = SourceSignature::from(&source);
        let mut catalog = catalog_sync::CatalogSync::default();
        let mut reconciled_source = None;
        let mut reconciled_at = None;
        let mut last_error = None;

        catalog
            .start(&source, &settings, &BTreeSet::new(), 1)
            .unwrap();
        let mut pending = Some((signature.clone(), 3));
        assert_eq!(
            finish_reconciliation(
                &mut pending,
                &catalog,
                3,
                &mut reconciled_source,
                &mut reconciled_at,
                &mut last_error,
            ),
            None
        );
        catalog.respond_for_test(json!({"ok":true,"revision":1,"generation":2,"next_sequence":0}));
        catalog.respond_for_test(json!({
            "ok":true,"revision":1,"generation":2,"complete":false,
            "cleanup_pending":false,"remove_stale":true,"remove_selection":true
        }));
        assert_eq!(
            finish_reconciliation(
                &mut pending,
                &catalog,
                3,
                &mut reconciled_source,
                &mut reconciled_at,
                &mut last_error,
            ),
            Some(false)
        );
        assert!(reconciled_source.is_none());
        assert!(reconciled_at.is_none());
        assert!(last_error.as_deref().unwrap().contains("catalog finish"));
        assert!(should_reconcile(
            &signature,
            reconciled_source.as_ref(),
            reconciled_at,
            false,
            Instant::now()
        ));

        catalog
            .start(&source, &settings, &BTreeSet::new(), 2)
            .unwrap();
        pending = Some((signature.clone(), 3));
        catalog.respond_for_test(json!({"ok":true,"revision":2,"generation":3,"next_sequence":0}));
        catalog.respond_for_test(json!({
            "ok":true,"revision":2,"generation":3,"complete":true,
            "cleanup_pending":false,"remove_stale":true,"remove_selection":true
        }));
        assert_eq!(
            finish_reconciliation(
                &mut pending,
                &catalog,
                3,
                &mut reconciled_source,
                &mut reconciled_at,
                &mut last_error,
            ),
            Some(false)
        );
        assert_eq!(reconciled_source, Some(signature));
        assert!(reconciled_at.is_some());
        assert_eq!(last_error, None);
    }

    #[test]
    fn artwork_changes_publish_only_after_current_catalog_finish() {
        let mut catalog = catalog_sync::CatalogSync::default();
        let mut artwork = artwork::ArtworkStore::default();
        artwork.update(&snapshot());
        let mut previous = BTreeMap::from([(10, "old".to_owned())]);
        let mut changed = snapshot();
        changed.artwork_revisions.insert(10, "new".to_owned());
        changed.games.retain(|game| game.id == 10);

        // A discovery and Steam read may succeed while the Catalog still rejects
        // the replacement. Keep the old readback authorization until Finish.
        catalog
            .start(&changed, &ProductSettings::default(), &BTreeSet::new(), 1)
            .unwrap();
        let mut pending = Some((changed.clone(), 3));
        publish_reconciled_artwork(&mut pending, &catalog, &mut artwork, &mut previous, 3, true);
        assert!(pending.is_some());
        assert_eq!(previous.get(&10).map(String::as_str), Some("old"));
        assert!(artwork.contains_game_for_test(7));

        catalog.respond_for_test(
            json!({"ok":false,"error":{"code":"catalog_error","message":"retry"}}),
        );
        assert!(!catalog.succeeded());
        pending = Some((changed.clone(), 3));
        publish_reconciled_artwork(&mut pending, &catalog, &mut artwork, &mut previous, 3, true);
        assert_eq!(previous.get(&10).map(String::as_str), Some("old"));
        assert!(artwork.contains_game_for_test(7));

        catalog
            .start(&changed, &ProductSettings::default(), &BTreeSet::new(), 2)
            .unwrap();
        catalog.respond_for_test(json!({"ok":true,"revision":1,"generation":2,"next_sequence":0}));
        catalog.respond_for_test(json!({"ok":true,"revision":2,"generation":2,"next_sequence":1}));
        catalog.respond_for_test(json!({
            "ok":true,"revision":3,"generation":2,"complete":true,
            "cleanup_pending":false,"remove_stale":true,"remove_selection":true
        }));
        assert!(catalog.succeeded());

        // A completed old transaction cannot republish after disable or a new epoch.
        pending = Some((changed.clone(), 3));
        publish_reconciled_artwork(
            &mut pending,
            &catalog,
            &mut artwork,
            &mut previous,
            4,
            false,
        );
        assert_eq!(previous.get(&10).map(String::as_str), Some("old"));
        assert!(artwork.contains_game_for_test(7));

        pending = Some((changed, 4));
        publish_reconciled_artwork(&mut pending, &catalog, &mut artwork, &mut previous, 4, true);
        assert_eq!(previous.get(&10).map(String::as_str), Some("new"));
        assert!(!artwork.contains_game_for_test(7));
    }

    #[test]
    fn stale_pre_disable_scan_cannot_publish_after_reenable() {
        let scan_start_settings = ProductSettings {
            enabled: true,
            auto_sync: true,
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

        let current_settings = ProductSettings {
            enabled: true,
            auto_sync: false,
            include_steam: true,
            autosync_remove_uninstalled: false,
            exclude_games: vec![Exclusion::Id("10".into())],
        };
        let now = Instant::now();
        let mut scan_pending = false;
        let mut next_auto_sync = None;
        schedule_settings_scan(
            &current_settings,
            &mut scan_pending,
            &mut next_auto_sync,
            now,
        );
        assert!(scan_is_due(
            &current_settings,
            scan_pending,
            next_auto_sync,
            now
        ));

        let (captured_settings, manual, settings_epoch, result) = worker.take_result().unwrap();
        assert!(!worker.busy());
        assert!(!manual);
        assert_eq!(settings_epoch, 3);
        let scan = CapturedScan {
            snapshot: result.unwrap(),
            settings: captured_settings,
            source_revision: 1,
            settings_epoch,
        };
        assert_eq!(scan.settings, scan_start_settings);
        assert!(scan.settings.include_steam);

        let mut catalog = catalog_sync::CatalogSync::default();
        // A Steam read may finish after disable and re-enable. Its old scan
        // must not reach the catalog even when the current settings are enabled.
        assert!(
            scan.start_catalog(&mut catalog, &[9001].into(), 5, true)
                .unwrap()
                .is_none()
        );
        assert!(!catalog.busy());
        assert_eq!(catalog.status()["last_import_count"], 0);
        reschedule_stale_scan(
            &current_settings,
            true,
            &mut scan_pending,
            &mut next_auto_sync,
            now,
        );
        assert!(scan_is_due(
            &current_settings,
            scan_pending,
            next_auto_sync,
            now
        ));

        let fresh = CapturedScan {
            snapshot: snapshot(),
            settings: current_settings,
            source_revision: 2,
            settings_epoch: 5,
        };
        let (count, _, _, _) = fresh
            .start_catalog(&mut catalog, &[9001].into(), 5, true)
            .unwrap()
            .unwrap();
        assert_eq!(catalog.status()["busy"], true);
        assert_eq!(count, 1);
        let catalog_status = catalog.status();
        assert_eq!(catalog_status["busy"], true);
        assert_eq!(catalog_status["last_import_count"], 1);
        assert_eq!(catalog_status["skipped_steam_count"], 1);
        assert_eq!(
            catalog.pending_payload_for_test()["action"],
            "provider_snapshot_begin"
        );
        catalog.respond_for_test(json!({"ok":true,"revision":1,"generation":2,"next_sequence":0}));
        let batch = catalog.pending_payload_for_test();
        assert_eq!(batch["entries"][0]["source_id"], "8");
        assert_eq!(batch["selection_remove_source_ids"], json!(["10", "7"]));
        catalog.respond_for_test(json!({"ok":true,"revision":2,"generation":2,"next_sequence":1}));
        let finish = catalog.pending_payload_for_test();
        assert_eq!(finish["remove_stale"], false);
        catalog.respond_for_test(json!({
            "ok":true,"revision":3,"generation":2,"complete":true,
            "cleanup_pending":false,"remove_stale":false,"remove_selection":true
        }));
        assert_eq!(catalog.status()["busy"], false);
        assert_eq!(catalog.status()["last_error"], Value::Null);

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
