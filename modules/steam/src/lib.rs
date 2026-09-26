//! Read-only local Steam provider. Product policy and launching stay in other libraries.
mod artwork;
mod catalog_sync;
mod configuration;
mod discovery;
mod keyvalues;
mod metadata;
mod settings;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use configuration::ProductConfiguration;
use discovery::{Game, Scan};
use serde::Deserialize;
use serde_json::{Value, json};
use settings::{Exclusion, ProductSettings, StartupSettings};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SERVICE: &str = "amaterasu.steam/1";
const MANAGEMENT_CALLER: &str = "amaterasu.management";

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Refresh {},
    ForceSync {},
    Status {},
    SettingsGet {},
    SettingsUpdate {
        enabled: bool,
        #[serde(default)]
        auto_sync: Option<bool>,
        exclude_games: Option<Vec<Exclusion>>,
        #[serde(default)]
        include_tools: Option<bool>,
        #[serde(default)]
        sync_all_installed: Option<bool>,
        #[serde(default)]
        recent_games: Option<u32>,
        #[serde(default)]
        recent_max_age_days: Option<u32>,
        #[serde(default)]
        autosync_remove_uninstalled: Option<bool>,
        expected_revision: u64,
    },
    List {
        revision: Option<u64>,
        after: Option<u32>,
        limit: Option<usize>,
    },
    ArtworkRead {
        app_id: u32,
        revision: String,
        offset: u64,
    },
}

#[derive(Default)]
struct Provider {
    enabled: bool,
    roots: Vec<PathBuf>,
    automatic_roots: bool,
    steam_executable: Option<PathBuf>,
    scan: Option<Scan>,
    games: BTreeMap<u32, Game>,
    revision: u64,
    snapshot_complete: bool,
    warnings: Vec<String>,
    warning_count: u64,
    appinfo_complete: bool,
    history_complete: bool,
    owned_history_complete: bool,
    remote_artwork: artwork::RemoteArtworkWorker,
    local_artwork: artwork::LocalArtworkWorker,
    local_artwork_responses: VecDeque<(u64, Value)>,
    artwork_validation_pending: bool,
    artwork_validation_complete: bool,
    artwork_validation_results: BTreeMap<u32, artwork::ArtworkMetadata>,
    artwork_verification_after: Option<u32>,
    artwork_verification_done: bool,
    artwork_validation_retry_at: Option<Instant>,
    upgrade_queue: VecDeque<u32>,
    upgrade_pending: Option<PendingArtworkUpgrade>,
    upgrade_serial: u64,
    artwork_generation: u64,
    #[cfg(test)]
    artwork_completion_count: usize,
}

#[derive(Clone)]
struct PendingArtworkUpgrade {
    serial: u64,
    generation: u64,
    app_id: u32,
    last_updated: u64,
    local_revision: Option<String>,
    candidate_signature: String,
}

impl Provider {
    fn new(configured_roots: Vec<PathBuf>) -> Self {
        let automatic_roots = configured_roots.is_empty();
        let roots = if automatic_roots {
            discovery::default_library_roots()
        } else {
            configured_roots
        };
        let steam_executable = discovery::find_steam_executable(&roots);
        Self {
            roots,
            automatic_roots,
            steam_executable,
            ..Self::default()
        }
    }

    fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        self.artwork_generation = self.artwork_generation.saturating_add(1);
        self.local_artwork.set_generation(self.artwork_generation);
        if !enabled {
            self.scan = None;
            self.artwork_validation_pending = false;
            self.artwork_validation_complete = false;
            self.artwork_validation_results.clear();
            self.artwork_verification_after = None;
            self.artwork_verification_done = false;
            self.artwork_validation_retry_at = None;
            self.games.clear();
            self.upgrade_queue.clear();
            self.upgrade_pending = None;
            self.warnings.clear();
            self.warning_count = 0;
            self.appinfo_complete = false;
            self.history_complete = false;
            self.owned_history_complete = false;
            self.revision = self.revision.saturating_add(1);
            self.snapshot_complete = false;
        }
    }

    fn start_scan(&mut self) -> bool {
        self.start_scan_with_defaults(discovery::default_library_roots)
    }

    fn start_scan_with_defaults(&mut self, defaults: impl FnOnce() -> Vec<PathBuf>) -> bool {
        if !self.enabled || self.scan.is_some() || self.revision == u64::MAX {
            return false;
        }
        if self.automatic_roots {
            self.roots = defaults();
        }
        self.artwork_generation = self.artwork_generation.saturating_add(1);
        self.local_artwork.set_generation(self.artwork_generation);
        self.upgrade_pending = None;
        self.upgrade_queue.clear();
        self.artwork_validation_pending = false;
        self.artwork_validation_complete = false;
        self.artwork_validation_results.clear();
        self.artwork_verification_after = None;
        self.artwork_verification_done = false;
        self.artwork_validation_retry_at = None;
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
        self.process_local_artwork();
        if self.scan.is_none()
            && self
                .artwork_validation_retry_at
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.artwork_validation_retry_at = None;
            self.start_scan();
        }
        if let Some(scan) = &mut self.scan {
            scan.tick();
        }
        if self.scan.as_ref().is_some_and(|scan| scan.finished)
            && !self.artwork_validation_pending
            && !self.artwork_validation_complete
        {
            let scan = self.scan.as_ref().expect("finished scan exists");
            if scan
                .games
                .values()
                .all(|game| game.artwork_source_path.is_none())
            {
                self.artwork_validation_complete = true;
            } else {
                let targets = scan
                    .games
                    .values()
                    .filter(|game| !self.artwork_validation_results.contains_key(&game.app_id))
                    .map(|game| artwork::LocalArtworkTarget {
                        app_id: game.app_id,
                        library: PathBuf::from(&game.library_directory),
                    })
                    .collect::<Vec<_>>();
                if targets.is_empty() {
                    self.artwork_validation_complete = true;
                } else {
                    match self
                        .local_artwork
                        .enqueue(artwork::LocalArtworkRequest::Validate {
                            generation: self.artwork_generation,
                            roots: self.roots.clone(),
                            targets,
                        }) {
                        Ok(()) => self.artwork_validation_pending = true,
                        Err("artwork_busy") => {}
                        Err(_) => {
                            self.scan = None;
                            self.artwork_validation_results.clear();
                            self.artwork_validation_retry_at =
                                Some(Instant::now() + Duration::from_secs(5 * 60));
                            self.warnings.push("artwork_validation_unavailable".into());
                            self.warning_count = self.warning_count.saturating_add(1);
                        }
                    }
                }
            }
        }
        if self.artwork_validation_complete && !self.artwork_verification_done {
            self.verify_artwork_batch();
        }
        if self.artwork_validation_complete
            && self.artwork_verification_done
            && self.scan.as_ref().is_some_and(|scan| scan.finished)
        {
            let mut scan = self.scan.take().expect("finished scan exists");
            self.artwork_validation_complete = false;
            self.artwork_verification_after = None;
            self.artwork_verification_done = false;
            if !self.artwork_validation_results.is_empty() {
                for (app_id, metadata) in std::mem::take(&mut self.artwork_validation_results) {
                    let Some(game) = scan.games.get_mut(&app_id) else {
                        continue;
                    };
                    game.artwork_source_path = metadata.source_path;
                    game.artwork_local_revision = metadata.revision.clone();
                    game.artwork_is_remote = metadata.revision.is_none();
                    game.artwork_revision = metadata
                        .revision
                        .or_else(|| artwork::remote_revision(app_id, game.last_updated));
                    game.artwork_candidate_signature = metadata.candidate_signature;
                    game.artwork_needs_remote_upgrade = metadata.needs_remote_upgrade;
                }
            }
            // The Steam client can be installed, moved, or removed while the
            // provider is running. Resolve it with each discovery snapshot so
            // reconciliation never keeps a stale launch program.
            self.steam_executable = discovery::find_steam_executable(&self.roots);
            self.artwork_generation = self.artwork_generation.saturating_add(1);
            self.local_artwork.set_generation(self.artwork_generation);
            self.upgrade_pending = None;
            let mut games = scan.games;
            for (app_id, game) in &mut games {
                if let Some(previous) = self.games.get(app_id)
                    && previous.artwork_is_remote
                    && previous.artwork_needs_remote_upgrade
                    && game.artwork_needs_remote_upgrade
                    && previous.last_updated == game.last_updated
                    && previous.artwork_local_revision == game.artwork_local_revision
                    && previous.artwork_candidate_signature == game.artwork_candidate_signature
                {
                    game.artwork_is_remote = true;
                    game.artwork_revision = previous.artwork_revision.clone();
                }
            }
            self.games = games;
            self.warnings = scan.warnings;
            self.warning_count = scan.warning_count;
            self.snapshot_complete = scan.warning_count == 0;
            self.upgrade_queue = if self.snapshot_complete {
                self.games
                    .iter()
                    .filter_map(|(app_id, game)| {
                        (game.installed
                            && game.artwork_needs_remote_upgrade
                            && !game.artwork_is_remote)
                            .then_some(*app_id)
                    })
                    .collect()
            } else {
                VecDeque::new()
            };
            self.appinfo_complete = scan.appinfo_complete;
            self.history_complete = scan.history_complete;
            self.owned_history_complete = scan.owned_history_complete;
            self.revision = self.revision.saturating_add(1);
        }
        self.enqueue_next_upgrade();
    }

    fn enqueue_next_upgrade(&mut self) {
        if self.upgrade_pending.is_some()
            || !self.enabled
            || !self.snapshot_complete
            || self.upgrade_serial == u64::MAX
        {
            return;
        }
        while let Some(app_id) = self.upgrade_queue.front().copied() {
            let Some(game) = self.games.get(&app_id) else {
                self.upgrade_queue.pop_front();
                continue;
            };
            if !game.installed || !game.artwork_needs_remote_upgrade || game.artwork_is_remote {
                self.upgrade_queue.pop_front();
                continue;
            }
            let Some(revision) = artwork::remote_revision(app_id, game.last_updated) else {
                self.upgrade_queue.pop_front();
                continue;
            };
            let request = artwork::RemoteArtworkRequest {
                prefetch: true,
                generation: self.artwork_generation,
                request_id: self.upgrade_serial.saturating_add(1),
                app_id,
                last_updated: game.last_updated,
                revision,
                offset: 0,
                deadline: Instant::now() + Duration::from_secs(10),
            };
            match self.remote_artwork.enqueue(request) {
                Ok(()) => {
                    self.upgrade_serial = self.upgrade_serial.saturating_add(1);
                    self.upgrade_pending = Some(PendingArtworkUpgrade {
                        serial: self.upgrade_serial,
                        generation: self.artwork_generation,
                        app_id,
                        last_updated: game.last_updated,
                        local_revision: game.artwork_local_revision.clone(),
                        candidate_signature: game.artwork_candidate_signature.clone(),
                    });
                    self.upgrade_queue.pop_front();
                    return;
                }
                Err("artwork_busy") => return,
                Err(_) => {
                    self.upgrade_queue.pop_front();
                }
            }
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
            Action::ForceSync {} => ActionResult::ForceSync,
            Action::Status {} => ActionResult::Immediate(self.status()),
            Action::SettingsGet {} => ActionResult::SettingsGet,
            Action::SettingsUpdate {
                enabled,
                auto_sync,
                exclude_games,
                include_tools,
                sync_all_installed,
                recent_games,
                recent_max_age_days,
                autosync_remove_uninstalled,
                expected_revision,
            } => ActionResult::SettingsUpdate {
                settings: ProductSettingsPatch {
                    enabled,
                    auto_sync,
                    exclude_games,
                    include_tools,
                    sync_all_installed,
                    recent_games,
                    recent_max_age_days,
                    autosync_remove_uninstalled,
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
            Action::ArtworkRead {
                app_id,
                revision,
                offset,
            } => ActionResult::ArtworkRead {
                app_id,
                revision,
                offset,
            },
        }
    }

    fn read_artwork(
        &mut self,
        request_id: u64,
        app_id: u32,
        revision: &str,
        offset: u64,
        remaining_ms: u64,
    ) -> Result<Option<Value>, &'static str> {
        if app_id == 0 {
            return Err("invalid_request");
        }
        let Some(game) = self.games.get(&app_id) else {
            return Err("artwork_unavailable");
        };
        if game.artwork_revision.as_deref() != Some(revision) {
            return Err("artwork_changed");
        }
        if game.artwork_is_remote {
            let last_updated = game.last_updated;
            if !offset.is_multiple_of(artwork::ARTWORK_CHUNK_BYTES as u64)
                || offset > artwork::MAX_ARTWORK_BYTES
            {
                return Err("invalid_cursor");
            }
            let deadline = Instant::now()
                .checked_add(Duration::from_millis(remaining_ms))
                .ok_or("invalid_request")?;
            self.remote_artwork.enqueue(artwork::RemoteArtworkRequest {
                prefetch: false,
                generation: self.artwork_generation,
                request_id,
                app_id,
                last_updated,
                revision: revision.to_owned(),
                offset,
                deadline,
            })?;
            return Ok(None);
        }
        let Some(source_path) = game.artwork_source_path.as_deref() else {
            return Err("artwork_unavailable");
        };

        let mut allowed_roots = vec![PathBuf::from(&game.library_directory)];
        allowed_roots.extend(self.roots.iter().cloned());
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(remaining_ms))
            .ok_or("invalid_request")?;
        self.local_artwork
            .enqueue(artwork::LocalArtworkRequest::Read {
                generation: self.artwork_generation,
                request_id,
                app_id,
                source_path: source_path.to_owned(),
                revision: revision.to_owned(),
                allowed_roots,
                offset,
                deadline,
            })?;
        Ok(None)
    }

    fn process_local_artwork(&mut self) {
        for completion in self.local_artwork.drain() {
            match completion {
                artwork::LocalArtworkCompletion::Validated {
                    generation,
                    artwork,
                } => {
                    if generation != self.artwork_generation || !self.artwork_validation_pending {
                        continue;
                    }
                    self.artwork_validation_pending = false;
                    let Some(scan) = self.scan.as_ref() else {
                        continue;
                    };
                    if artwork
                        .keys()
                        .any(|app_id| !scan.games.contains_key(app_id))
                    {
                        self.scan = None;
                        self.artwork_validation_results.clear();
                        self.artwork_validation_retry_at =
                            Some(Instant::now() + Duration::from_secs(5 * 60));
                        continue;
                    }
                    self.artwork_validation_results.extend(artwork);
                    self.artwork_validation_complete =
                        self.artwork_validation_results.len() == scan.games.len();
                }
                artwork::LocalArtworkCompletion::Read {
                    generation,
                    request_id,
                    app_id,
                    revision,
                    offset,
                    result,
                } => {
                    let current = generation == self.artwork_generation
                        && self.games.get(&app_id).is_some_and(|game| {
                            !game.artwork_is_remote
                                && game.artwork_revision.as_deref() == Some(revision.as_str())
                        });
                    let payload = if !current {
                        json!({"ok":false,"code":"artwork_changed"})
                    } else {
                        match result {
                            Ok(chunk) => artwork_chunk_payload(app_id, offset, chunk),
                            Err(code) => json!({"ok":false,"code":code}),
                        }
                    };
                    self.local_artwork_responses
                        .push_back((request_id, payload));
                }
            }
        }
    }

    fn verify_artwork_batch(&mut self) {
        let after = self.artwork_verification_after.unwrap_or(0);
        let batch = self
            .artwork_validation_results
            .range((std::ops::Bound::Excluded(after), std::ops::Bound::Unbounded))
            .take(17)
            .map(|(app_id, metadata)| (*app_id, metadata.clone()))
            .collect::<Vec<_>>();
        for (app_id, metadata) in batch.iter().take(16) {
            let Some(source_path) = metadata.source_path.as_deref() else {
                continue;
            };
            let current = metadata.revision.as_deref().is_some_and(|revision| {
                self.scan
                    .as_ref()
                    .and_then(|scan| scan.games.get(app_id))
                    .is_some_and(|game| {
                        let mut roots = vec![PathBuf::from(&game.library_directory)];
                        roots.extend(self.roots.iter().cloned());
                        artwork::local_revision_is_current(source_path, revision, &roots)
                    })
            });
            if !current {
                self.scan = None;
                self.artwork_validation_complete = false;
                self.artwork_validation_results.clear();
                self.artwork_verification_after = None;
                self.artwork_validation_retry_at = Some(Instant::now() + Duration::from_secs(1));
                return;
            }
        }
        if let Some((app_id, _)) = batch.iter().take(16).next_back() {
            self.artwork_verification_after = Some(*app_id);
        }
        self.artwork_verification_done = batch.len() <= 16;
    }

    fn drain_local_artwork(&mut self) -> Vec<(u64, Value)> {
        self.local_artwork_responses.drain(..).collect()
    }

    fn drain_remote_artwork(&mut self) -> Vec<(u64, Value)> {
        let mut responses = Vec::new();
        for completion in self.remote_artwork.drain() {
            #[cfg(test)]
            {
                self.artwork_completion_count += 1;
            }
            if completion.prefetch {
                let pending = self
                    .upgrade_pending
                    .as_ref()
                    .filter(|pending| {
                        pending.serial == completion.request_id
                            && pending.app_id == completion.app_id
                            && pending.generation == self.artwork_generation
                            && pending.generation == completion.generation
                    })
                    .cloned();
                if pending.is_some() {
                    self.upgrade_pending = None;
                }
                if completion.result.is_ok()
                    && pending.is_some()
                    && let Some(game) = self.games.get_mut(&completion.app_id)
                    && pending.as_ref().is_some_and(|pending| {
                        pending.last_updated == game.last_updated
                            && pending.local_revision == game.artwork_local_revision
                            && pending.candidate_signature == game.artwork_candidate_signature
                    })
                    && game.artwork_needs_remote_upgrade
                    && !game.artwork_is_remote
                    && artwork::remote_revision(game.app_id, game.last_updated).as_deref()
                        == Some(completion.revision.as_str())
                    && self.revision < u64::MAX
                {
                    game.artwork_is_remote = true;
                    game.artwork_revision = Some(completion.revision);
                    self.revision += 1;
                }
                continue;
            }
            let still_current = self.games.get(&completion.app_id).is_some_and(|game| {
                completion.generation == self.artwork_generation
                    && game.artwork_is_remote
                    && game.artwork_revision.as_deref() == Some(&completion.revision)
            });
            let source_failed =
                completion.result.as_ref().err().is_some_and(|code| {
                    matches!(*code, "artwork_unavailable" | "unsupported_artwork")
                });
            let mut payload = if !still_current {
                json!({"ok":false,"code":"artwork_changed"})
            } else {
                match completion.result {
                    Ok(chunk) => artwork_chunk_payload(completion.app_id, completion.offset, chunk),
                    Err(code) => json!({"ok":false,"code":code}),
                }
            };
            if still_current
                && source_failed
                && let Some(game) = self.games.get_mut(&completion.app_id)
                && let Some(local_revision) = &game.artwork_local_revision
                && self.revision < u64::MAX
            {
                game.artwork_revision = Some(local_revision.clone());
                game.artwork_is_remote = false;
                self.revision += 1;
                payload = json!({"ok":false,"code":"artwork_changed"});
            }
            responses.push((completion.request_id, payload));
        }
        responses
    }

    fn status(&self) -> Value {
        json!({"ok":true,"enabled":self.enabled,"scanning":self.scan.is_some(),"revision":self.revision,
            "game_count":self.games.len(),"warning_count":self.warning_count,"warnings":self.warnings,
            "complete":self.has_complete_snapshot(),
            "catalog_sources":{"appinfo":self.appinfo_complete,"play_history":self.history_complete,"owned_history":self.owned_history_complete},
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
            "scanning":self.scan.is_some(),"complete":self.has_complete_snapshot(),
            "catalog_sources":{"appinfo":self.appinfo_complete,"play_history":self.history_complete,"owned_history":self.owned_history_complete}})
    }
}

fn artwork_chunk_payload(app_id: u32, offset: u64, chunk: artwork::ArtworkChunk) -> Value {
    json!({
        "ok":true,
        "app_id":app_id,
        "revision":chunk.revision,
        "offset":offset,
        "next_offset":chunk.next_offset,
        "total_bytes":chunk.total_bytes,
        "mime_type":chunk.mime_type,
        "data":chunk.data
    })
}

enum ActionResult {
    Immediate(Value),
    ForceSync,
    ArtworkRead {
        app_id: u32,
        revision: String,
        offset: u64,
    },
    SettingsGet,
    SettingsUpdate {
        settings: ProductSettingsPatch,
        expected_revision: u64,
    },
}

struct ProductSettingsPatch {
    enabled: bool,
    auto_sync: Option<bool>,
    exclude_games: Option<Vec<Exclusion>>,
    include_tools: Option<bool>,
    sync_all_installed: Option<bool>,
    recent_games: Option<u32>,
    recent_max_age_days: Option<u32>,
    autosync_remove_uninstalled: Option<bool>,
}

impl ProductSettingsPatch {
    fn apply(self, current: &ProductSettings) -> ProductSettings {
        ProductSettings {
            enabled: self.enabled,
            auto_sync: self.auto_sync.unwrap_or(current.auto_sync),
            exclude_games: self
                .exclude_games
                .unwrap_or_else(|| current.exclude_games.clone()),
            include_tools: self.include_tools.unwrap_or(current.include_tools),
            sync_all_installed: self
                .sync_all_installed
                .unwrap_or(current.sync_all_installed),
            recent_games: self.recent_games.unwrap_or(current.recent_games),
            recent_max_age_days: self
                .recent_max_age_days
                .unwrap_or(current.recent_max_age_days),
            autosync_remove_uninstalled: self
                .autosync_remove_uninstalled
                .unwrap_or(current.autosync_remove_uninstalled),
        }
    }
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
                provider
                    .remote_artwork
                    .set_cache_root(settings.steam_artwork_cache_root);
                provider.set_enabled(configuration.settings().enabled);
                provider
            },
            configuration,
            catalog_sync: catalog_sync::CatalogSync::new(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.provider.tick();
        for (request, payload) in self.provider.drain_local_artwork() {
            self.complete(
                request,
                with_catalog_sync_status(
                    with_settings_ready(payload, self.configuration.is_ready()),
                    self.catalog_sync.status(),
                ),
            )?;
        }
        for (request, payload) in self.provider.drain_remote_artwork() {
            self.complete(
                request,
                with_catalog_sync_status(
                    with_settings_ready(payload, self.configuration.is_ready()),
                    self.catalog_sync.status(),
                ),
            )?;
        }
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
        let is_management_action = matches!(
            &action,
            Action::SettingsGet {}
                | Action::SettingsUpdate { .. }
                | Action::ArtworkRead { .. }
                | Action::ForceSync {}
        );
        if is_management_action && !is_management_caller(caller) {
            return self.complete(id, json!({"ok":false,"code":"forbidden"}));
        }
        let is_settings_action = matches!(
            &action,
            Action::SettingsGet {} | Action::SettingsUpdate { .. }
        );
        if self.configuration.is_busy() && is_settings_action {
            let code = if self.configuration.is_ready() {
                "settings_busy"
            } else {
                "settings_loading"
            };
            return self.complete(id, json!({"ok":false,"code":code}));
        }
        if !self.configuration.is_ready()
            && matches!(&action, Action::Refresh {} | Action::ForceSync {})
        {
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
            ActionResult::ForceSync => {
                match self
                    .catalog_sync
                    .request_force_sync(&mut self.provider, self.configuration.settings())
                {
                    Ok(()) => {
                        let mut payload = self.provider.status();
                        if let Some(object) = payload.as_object_mut() {
                            object.insert("ok".into(), json!(true));
                            object.insert("accepted".into(), json!(true));
                            object.insert("force_sync_pending".into(), json!(true));
                        }
                        self.complete(
                            id,
                            with_catalog_sync_status(
                                with_settings_ready(payload, self.configuration.is_ready()),
                                self.catalog_sync.status(),
                            ),
                        )
                    }
                    Err(code) => self.complete(id, json!({"ok":false,"code":code})),
                }
            }
            ActionResult::SettingsGet => {
                self.complete(id, self.configuration.management_response())
            }
            ActionResult::ArtworkRead {
                app_id,
                revision,
                offset,
            } => match self
                .provider
                .read_artwork(id, app_id, &revision, offset, remaining_ms)
            {
                Ok(Some(payload)) => self.complete(
                    id,
                    with_catalog_sync_status(
                        with_settings_ready(payload, self.configuration.is_ready()),
                        self.catalog_sync.status(),
                    ),
                ),
                Ok(None) => Ok(()),
                Err(code) => self.complete(
                    id,
                    with_catalog_sync_status(
                        with_settings_ready(
                            json!({"ok":false,"code":code}),
                            self.configuration.is_ready(),
                        ),
                        self.catalog_sync.status(),
                    ),
                ),
            },
            ActionResult::SettingsUpdate {
                settings: patch,
                expected_revision,
            } => {
                let settings = patch.apply(self.configuration.settings());
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
        object.insert(
            "force_sync_pending".into(),
            json!(status["force_sync_pending"].as_bool().unwrap_or(false)),
        );
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
                ActionResult::ForceSync => json!({"ok":false,"code":"management_only"}),
                ActionResult::ArtworkRead { .. } => {
                    json!({"ok":false,"code":"async_only"})
                }
                ActionResult::SettingsGet | ActionResult::SettingsUpdate { .. } => {
                    json!({"ok":false,"code":"management_only"})
                }
            },
            Err(_) => json!({"ok":false,"code":"invalid_request"}),
        }
    }
}

fn is_management_caller(caller: &str) -> bool {
    caller == MANAGEMENT_CALLER
}

amaterasu_module_sdk::export_module!(SteamModule);

#[cfg(test)]
mod tests;
