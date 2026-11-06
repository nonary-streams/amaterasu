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
use std::collections::{BTreeMap, VecDeque};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SERVICE: &str = "amaterasu.playnite/1";
const MANAGEMENT_CALLER: &str = "amaterasu.management";
const LIBRARY_CALLER: &str = "amaterasu.library";
const LAUNCH_CALLER: &str = "amaterasu.launch";
const AUTO_SYNC_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Status {},
    Refresh {},
    ArtworkRead {
        game_id: String,
        revision: String,
        offset: usize,
        transfer_id: String,
    },
    ArtworkRelease {
        transfer_id: String,
    },
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
    processes: Vec<discovery::ProcessImage>,
}

type SunshineObservation = (Vec<discovery::ProcessImage>, bool, u64);

struct PendingSunshineObservation {
    desired: SunshineObservation,
    acknowledgement: Receiver<()>,
    deadline: Instant,
}

#[derive(Default)]
struct SunshineConnectionObservation {
    confirmed: Option<SunshineObservation>,
    pending: Option<PendingSunshineObservation>,
}

impl SunshineConnectionObservation {
    fn confirmed_for(&self, desired: &SunshineObservation) -> bool {
        self.confirmed.as_ref() == Some(desired)
    }

    fn advance(
        &mut self,
        desired: SunshineObservation,
        submit: impl FnOnce(&SunshineObservation) -> Option<Receiver<()>>,
    ) {
        if let Some(pending) = self.pending.take()
            && pending.desired == desired
        {
            match pending.acknowledgement.try_recv() {
                Ok(()) => {
                    self.confirmed = Some(desired);
                    return;
                }
                Err(mpsc::TryRecvError::Empty) if Instant::now() < pending.deadline => {
                    self.pending = Some(pending);
                    return;
                }
                Err(_) => {}
            }
        }
        if self.confirmed_for(&desired) {
            return;
        }
        self.confirmed = None;
        if let Some(acknowledgement) = submit(&desired) {
            self.pending = Some(PendingSunshineObservation {
                desired,
                acknowledgement,
                deadline: Instant::now() + connector::OBSERVE_TIMEOUT,
            });
        }
    }
}

impl SnapshotWorker {
    fn start(settings_epoch: u64, processes: Vec<discovery::ProcessImage>) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let request_processes = processes.clone();
        let worker = thread::spawn(move || {
            let _ = sender.send(connector::request_snapshot(&request_processes));
        });
        Self {
            receiver: Some(receiver),
            _thread: Some(worker),
            settings_epoch,
            processes,
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

    fn retain_for_settings_epoch(&mut self, settings_epoch: u64, same_catalog_policy: bool) {
        if same_catalog_policy && self.busy() {
            self.settings_epoch = settings_epoch;
        }
    }
}

struct PlayniteModule {
    runtime: RuntimeClient,
    library_principal: String,
    configuration: ProductConfiguration,
    catalog: catalog_sync::CatalogSync,
    snapshot_worker: SnapshotWorker,
    pending_unsolicited_snapshot: Option<connector::SnapshotEvent>,
    sunshine_observation: SunshineConnectionObservation,
    settings_epoch: u64,
    observed_settings_epoch: u64,
    observed_settings: Option<ProductSettings>,
    next_auto_sync: Option<Instant>,
    last_error: Option<String>,
    last_snapshot_games: usize,
    last_discovery: discovery::Discovery,
    eligible_launch_ids: std::collections::BTreeSet<String>,
    catalog_processes: Option<Vec<discovery::ProcessImage>>,
    launches: launch::Launches,
    fullscreen_launches: fullscreen::FullscreenSessions,
    fullscreen_entry_sync: fullscreen_entry::FullscreenEntrySync,
    next_discovery: Instant,
    pending_launches: VecDeque<PendingLaunch>,
    binary_artwork: amaterasu_module_sdk::artwork_transfer::Source,
    binary_requests: BTreeMap<u64, BinaryArtwork>,
    binary_releases: BTreeMap<String, BinaryRelease>,
    artwork_revisions: BTreeMap<String, String>,
    artwork_snapshot_current: bool,
    pending_artwork: Vec<PendingArtwork>,
    pending_artwork_releases: Vec<PendingArtworkRelease>,
    last_epoch_ready: bool,
    #[cfg(test)]
    discovery_override: Option<discovery::Discovery>,
    #[cfg(test)]
    discovery_call_count: std::cell::Cell<usize>,
}

struct BinaryArtwork {
    owner: String,
    bytes: Vec<u8>,
    mime: Option<String>,
    total: Option<usize>,
    deadline: Instant,
}
struct BinaryRelease {
    processes: Vec<discovery::ProcessImage>,
    receiver: Option<Receiver<Result<(), String>>>,
    retry_at: Instant,
}
struct PendingArtworkRelease {
    request: u64,
    transfer_id: String,
    receiver: Receiver<Result<(), String>>,
}

struct PendingArtwork {
    request: u64,
    game_id: String,
    revision: String,
    transfer_id: String,
    catalog_reference: bool,
    processes: Vec<discovery::ProcessImage>,
    settings_epoch: u64,
    receiver: Receiver<Result<connector::ArtworkChunk, String>>,
}

struct PendingLaunch {
    requests: Vec<u64>,
    action: Action,
    operation_id: String,
    processes: Vec<discovery::ProcessImage>,
    settings_epoch: u64,
    deadline: Instant,
}

fn queue_pending_launch(
    pending_launches: &mut VecDeque<PendingLaunch>,
    mut incoming: PendingLaunch,
) -> Result<(), &'static str> {
    if let Some(existing) = pending_launches
        .iter_mut()
        .find(|pending| pending.operation_id == incoming.operation_id)
    {
        if existing.action != incoming.action {
            return Err("operation_conflict");
        }
        if existing.requests.len() >= 16 {
            return Err("launch_capacity");
        }
        existing.requests.append(&mut incoming.requests);
        return Ok(());
    }
    if pending_launches.len() >= 16 {
        return Err("launch_capacity");
    }
    pending_launches.push_back(incoming);
    Ok(())
}

impl ManagedModule for PlayniteModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("Playnite startup settings unavailable".into());
        };
        let library_principal = library_principal_from_settings(&payload)?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        let binary_artwork = amaterasu_module_sdk::artwork_transfer::Source::new()?;
        // SAFETY: dependencies retain this module and its registered factory.
        unsafe {
            binary_artwork.register(&runtime, SERVICE)?;
        }
        connector::start_sunshine_actor();
        Ok(Self {
            runtime,
            library_principal,
            configuration: ProductConfiguration::new(),
            catalog: catalog_sync::CatalogSync::default(),
            snapshot_worker: SnapshotWorker::default(),
            pending_unsolicited_snapshot: None,
            sunshine_observation: SunshineConnectionObservation::default(),
            settings_epoch: 0,
            observed_settings_epoch: u64::MAX,
            observed_settings: None,
            next_auto_sync: None,
            last_error: None,
            last_snapshot_games: 0,
            last_discovery: discovery::Discovery::default(),
            eligible_launch_ids: std::collections::BTreeSet::new(),
            catalog_processes: None,
            launches: launch::Launches::default(),
            fullscreen_launches: fullscreen::FullscreenSessions::default(),
            fullscreen_entry_sync: fullscreen_entry::FullscreenEntrySync::default(),
            next_discovery: Instant::now(),
            pending_launches: VecDeque::new(),
            binary_artwork,
            binary_requests: BTreeMap::new(),
            binary_releases: BTreeMap::new(),
            artwork_revisions: BTreeMap::new(),
            artwork_snapshot_current: false,
            pending_artwork: Vec::new(),
            pending_artwork_releases: Vec::new(),
            last_epoch_ready: false,
            #[cfg(test)]
            discovery_override: None,
            #[cfg(test)]
            discovery_call_count: std::cell::Cell::new(0),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.binary_artwork.reap();
        self.poll_binary_releases();
        self.configuration
            .advance(&self.runtime, &mut self.settings_epoch)?;
        self.observe_settings();
        self.launches.advance();
        self.refresh_discovery();
        self.observe_sunshine_connection();
        self.fullscreen_launches
            .advance(Instant::now(), &self.last_discovery.processes);
        self.poll_snapshot();
        self.poll_artwork()?;
        self.start_automatic_snapshot();
        self.catalog
            .advance(&self.runtime, self.settings_epoch)
            .map_err(|error| format!("Playnite catalog import failed: {error}"))?;
        let epoch_ready = self.catalog.epoch_ready();
        if !epoch_ready {
            self.eligible_launch_ids.clear();
            self.catalog_processes = None;
        } else if !self.last_epoch_ready
            && self.configuration.ready()
            && self.configuration.settings().enabled
            && self.configuration.settings().auto_sync
        {
            self.next_auto_sync = Some(Instant::now());
        }
        self.last_epoch_ready = epoch_ready;
        self.configuration
            .release_deferred_response(&self.runtime, self.catalog.epoch_ready())?;
        self.resume_pending_launch()?;
        self.poll_unsolicited_snapshot();
        self.fullscreen_entry_sync.advance(&self.runtime)?;
        if let Some(launchable_game_ids) = self.catalog.take_completed_launchable_game_ids()
            && self.catalog.epoch_ready()
            && can_publish_eligibility(self.catalog_processes.as_deref(), &self.last_discovery)
        {
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

impl Drop for PlayniteModule {
    fn drop(&mut self) {
        connector::stop_sunshine_actor();
    }
}

impl PlayniteModule {
    fn discover_processes(&self) -> discovery::Discovery {
        #[cfg(test)]
        self.discovery_call_count
            .set(self.discovery_call_count.get() + 1);
        #[cfg(test)]
        if let Some(discovery) = &self.discovery_override {
            return discovery.clone();
        }
        discovery::discover()
    }

    fn observe_sunshine_connection(&mut self) {
        let enabled = self.configuration.ready() && self.configuration.settings().enabled;
        let desired = (
            self.last_discovery.processes.clone(),
            enabled,
            self.settings_epoch,
        );
        self.sunshine_observation.advance(desired, |desired| {
            connector::observe_sunshine_processes(&desired.0, desired.1, desired.2)
        });
    }

    fn observe_settings(&mut self) {
        if !self.configuration.ready() || self.observed_settings_epoch == self.settings_epoch {
            return;
        }
        self.observed_settings_epoch = self.settings_epoch;
        let settings = self.configuration.settings().clone();
        let same_catalog_policy = apply_settings_transition(
            self.observed_settings.as_ref(),
            &settings,
            &mut self.eligible_launch_ids,
            &mut self.catalog_processes,
            &mut self.next_auto_sync,
            Instant::now(),
        );
        if !same_catalog_policy {
            self.artwork_revisions.clear();
            self.binary_artwork.clear();
            self.artwork_snapshot_current = false;
            self.catalog.revoke_epoch();
            self.configuration.require_epoch_for_deferred_response();
        }
        // A snapshot from this Playnite instance remains usable when only
        // focus or Fullscreen entry behavior changed while it was in flight.
        self.snapshot_worker
            .retain_for_settings_epoch(self.settings_epoch, same_catalog_policy);
        self.catalog
            .retain_for_settings_epoch(self.settings_epoch, same_catalog_policy);
        self.observed_settings = Some(settings.clone());
        if !settings.enabled || !settings.auto_sync || !same_catalog_policy {
            self.pending_unsolicited_snapshot = None;
        }
        self.fullscreen_entry_sync
            .set_desired(cfg!(windows) && settings.enabled && settings.fullscreen_entry_enabled);
    }

    fn refresh_discovery(&mut self) {
        let now = Instant::now();
        if now < self.next_discovery {
            return;
        }
        let previous_processes = self.last_discovery.processes.clone();
        let discovered = self.discover_processes();
        refresh_discovery_at(
            &mut self.last_discovery,
            &mut self.next_discovery,
            now,
            false,
            || discovered,
        );
        if update_discovery_state(
            &previous_processes,
            &self.last_discovery.processes,
            &mut self.eligible_launch_ids,
            &mut self.next_auto_sync,
            self.configuration.ready()
                && self.configuration.settings().enabled
                && self.configuration.settings().auto_sync,
            Instant::now(),
        ) {
            self.artwork_revisions.clear();
            self.binary_artwork.clear();
            self.artwork_snapshot_current = false;
            self.catalog.cancel_after_process_change();
            self.catalog.revoke_epoch();
            self.pending_unsolicited_snapshot = None;
        }
    }

    fn refresh_discovery_now(&mut self) {
        // Launch and stop need current PIDs after a Playnite restart. Recovery
        // also checks paths against the launch's trusted install root.
        let previous_processes = self.last_discovery.processes.clone();
        let discovered = self.discover_processes();
        refresh_discovery_at(
            &mut self.last_discovery,
            &mut self.next_discovery,
            Instant::now(),
            true,
            || discovered,
        );
        if update_discovery_state(
            &previous_processes,
            &self.last_discovery.processes,
            &mut self.eligible_launch_ids,
            &mut self.next_auto_sync,
            self.configuration.ready()
                && self.configuration.settings().enabled
                && self.configuration.settings().auto_sync,
            Instant::now(),
        ) {
            self.artwork_revisions.clear();
            self.binary_artwork.clear();
            self.artwork_snapshot_current = false;
            self.catalog.cancel_after_process_change();
            self.catalog.revoke_epoch();
            self.pending_unsolicited_snapshot = None;
        }
        self.observe_sunshine_connection();
    }

    fn poll_snapshot(&mut self) {
        let Some(result) = self.snapshot_worker.take_result() else {
            return;
        };
        if !self.configuration.ready()
            || !self.configuration.settings().enabled
            || !self.catalog.epoch_ready()
            || self.snapshot_worker.settings_epoch != self.settings_epoch
        {
            self.last_error = Some("discarded a Playnite snapshot after settings changed".into());
            return;
        }
        if !snapshot_processes_match_instance(&self.snapshot_worker.processes, &self.last_discovery)
        {
            self.last_error =
                Some("discarded a Playnite snapshot after its process changed".into());
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
                    Ok(()) => {
                        update_artwork_revisions(&mut self.artwork_revisions, &snapshot);
                        self.artwork_snapshot_current = true;
                        self.catalog_processes = Some(self.snapshot_worker.processes.clone());
                        self.last_error = None;
                    }
                    Err(error) => self.last_error = Some(error),
                }
            }
            Err(error) => self.last_error = Some(error),
        }
    }

    fn poll_unsolicited_snapshot(&mut self) {
        if let Some(snapshot) = connector::take_unsolicited_snapshot() {
            self.pending_unsolicited_snapshot = Some(snapshot);
        }
        if !self.configuration.ready() {
            self.pending_unsolicited_snapshot = None;
            return;
        }
        let Some(event) = self.pending_unsolicited_snapshot.as_ref() else {
            return;
        };
        if !unsolicited_snapshot_is_current(
            event,
            self.configuration.settings(),
            self.settings_epoch,
            &self.last_discovery,
        ) {
            self.pending_unsolicited_snapshot = None;
            return;
        }
        if self.catalog.busy() || self.snapshot_worker.busy() {
            return;
        }
        if !self.catalog.epoch_ready() {
            return;
        }
        let event = self.pending_unsolicited_snapshot.take().unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |value| value.as_secs() as i64);
        match self.catalog.start(
            &event.snapshot,
            self.configuration.settings(),
            self.settings_epoch,
            now,
        ) {
            Ok(()) => {
                update_artwork_revisions(&mut self.artwork_revisions, &event.snapshot);
                self.artwork_snapshot_current = true;
                self.last_snapshot_games = event.snapshot.games.len();
                self.catalog_processes = Some(event.processes);
                self.next_auto_sync = Some(Instant::now() + AUTO_SYNC_INTERVAL);
                self.last_error = None;
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
            || !self.catalog.epoch_ready()
            || self.snapshot_worker.busy()
            || self.catalog.busy()
            || !self.sunshine_observation.confirmed_for(&(
                self.last_discovery.processes.clone(),
                true,
                self.settings_epoch,
            ))
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
        if payload.get("action").and_then(Value::as_str) == Some("artwork_open") {
            if caller != self.library_principal && caller != MANAGEMENT_CALLER {
                return self.complete(id, json!({"ok":false,"code":"forbidden"}));
            }
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Open {
                action: String,
                game_id: String,
                revision: String,
            }
            let Ok(open) = serde_json::from_value::<Open>(payload) else {
                return self.complete(id, json!({"ok":false,"code":"invalid_request"}));
            };
            let _ = open.action;
            if self.binary_requests.len() + self.binary_releases.len() >= 4 {
                return self.complete(id, json!({"ok":false,"code":"artwork_busy"}));
            }
            let mut random = [0u8; 16];
            getrandom::fill(&mut random).map_err(|_| "artwork entropy unavailable")?;
            let token: String = random.iter().map(|b| format!("{b:02x}")).collect();
            self.binary_requests.insert(
                id,
                BinaryArtwork {
                    owner: caller.into(),
                    bytes: Vec::new(),
                    mime: None,
                    total: None,
                    deadline: Instant::now() + Duration::from_secs(25),
                },
            );
            self.request_artwork(
                id,
                open.game_id,
                open.revision,
                0,
                token,
                caller == self.library_principal,
            )?;
            if !self.pending_artwork.iter().any(|item| item.request == id) {
                self.binary_requests.remove(&id);
            }
            return Ok(());
        }
        if payload
            .get("action")
            .and_then(Value::as_str)
            .is_some_and(|action| matches!(action, "artwork_read" | "artwork_release"))
        {
            return self.complete(id, json!({"ok":false,"code":"invalid_request"}));
        }
        let action = match serde_json::from_value::<Action>(payload) {
            Ok(action) => action,
            Err(_) => return self.complete(id, json!({"ok":false,"code":"invalid_request"})),
        };
        if action.is_settings() && !settings_caller_is_authorized(caller) {
            return self.complete(id, json!({"ok":false,"code":"forbidden"}));
        }
        let artwork_read = matches!(
            action,
            Action::ArtworkRead { .. } | Action::ArtworkRelease { .. }
        );
        if !artwork_caller_allowed(caller, &self.library_principal, artwork_read) {
            return self.complete(id, json!({"ok":false,"code":"forbidden"}));
        }
        if let Action::ArtworkRead {
            game_id,
            revision,
            offset,
            transfer_id,
        } = &action
        {
            return self.request_artwork(
                id,
                game_id.clone(),
                revision.clone(),
                *offset,
                transfer_id.clone(),
                caller == self.library_principal,
            );
        }
        if action.is_launch() && !launch_caller_is_authorized(caller) {
            return self.complete(id, json!({"ok":false,"code":"forbidden"}));
        }
        if action.is_fullscreen_launch() && !launch_caller_is_authorized(caller) {
            return self.complete(id, json!({"ok":false,"code":"forbidden"}));
        }
        if let Some(operation_id) = match &action {
            Action::LaunchStart { operation_id, .. } | Action::FullscreenStart { operation_id } => {
                Some(operation_id.clone())
            }
            _ => None,
        } {
            if !self.configuration.ready() {
                return self.complete(id, launch::not_started_error("settings_loading"));
            }
            if !self.configuration.settings().enabled {
                return self.complete(id, launch::not_started_error("playnite_disabled"));
            }
            self.refresh_discovery_now();
            if !self.pending_launches.is_empty() || !self.catalog.verify_launch_epoch(&operation_id)
            {
                let incoming = PendingLaunch {
                    requests: vec![id],
                    action,
                    operation_id,
                    processes: self.last_discovery.processes.clone(),
                    settings_epoch: self.settings_epoch,
                    deadline: Instant::now() + Duration::from_secs(4),
                };
                return match queue_pending_launch(&mut self.pending_launches, incoming) {
                    Ok(()) => Ok(()),
                    Err(code) => self.complete(id, launch::not_started_error(code)),
                };
            }
        }
        self.handle_action(id, action)
    }

    fn resume_pending_launch(&mut self) -> ModuleResult<()> {
        let Some(pending) = self.pending_launches.pop_front() else {
            return Ok(());
        };
        if Instant::now() >= pending.deadline {
            self.catalog
                .cancel_launch_probe(Some(&pending.operation_id));
            for request in pending.requests {
                self.complete(request, launch::not_started_error("catalog_unavailable"))?;
            }
            return Ok(());
        }
        self.refresh_discovery_now();
        if !self.configuration.ready()
            || !self.configuration.settings().enabled
            || pending.settings_epoch != self.settings_epoch
            || pending.processes != self.last_discovery.processes
        {
            self.catalog
                .cancel_launch_probe(Some(&pending.operation_id));
            for request in pending.requests {
                self.complete(request, launch::not_started_error("playnite_changed"))?;
            }
            return Ok(());
        }
        if !self.catalog.verify_launch_epoch(&pending.operation_id) {
            self.pending_launches.push_front(pending);
            return Ok(());
        }
        for request in pending.requests {
            self.handle_action(request, pending.action.clone())?;
        }
        Ok(())
    }

    fn handle_action(&mut self, id: u64, action: Action) -> ModuleResult<()> {
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
            Action::ArtworkRead {
                game_id,
                revision,
                offset,
                transfer_id,
            } => self.request_artwork(id, game_id, revision, offset, transfer_id, false),
            Action::ArtworkRelease { transfer_id } => {
                if !connector::valid_artwork_transfer_id(&transfer_id) {
                    return self.complete(id, json!({"ok":false,"code":"invalid_cursor"}));
                }
                if self.pending_artwork_releases.len() >= 8 {
                    return self.complete(id, json!({"ok":false,"code":"artwork_busy"}));
                }
                match connector::release_artwork_transfer(
                    &self.last_discovery.processes,
                    &transfer_id,
                ) {
                    Ok(receiver) => {
                        self.pending_artwork_releases.push(PendingArtworkRelease {
                            request: id,
                            transfer_id,
                            receiver,
                        });
                        Ok(())
                    }
                    Err(_) => self.complete(id, json!({"ok":false,"code":"artwork_unavailable"})),
                }
            }
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
                self.refresh_discovery_now();
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
                self.refresh_discovery_now();
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
                self.refresh_discovery_now();
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
                self.refresh_discovery_now();
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
                if self.snapshot_worker.busy() || self.catalog.busy() || !self.catalog.epoch_ready()
                {
                    return self.complete(id, json!({"ok":false,"code":"refresh_busy"}));
                }
                self.refresh_discovery_now();
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

    fn request_artwork(
        &mut self,
        request: u64,
        game_id: String,
        revision: String,
        offset: usize,
        transfer_id: String,
        catalog_reference: bool,
    ) -> ModuleResult<()> {
        let game_id = game_id.to_ascii_lowercase();
        if !self.configuration.ready() || !self.configuration.settings().enabled {
            return self.complete(request, json!({"ok":false,"code":"artwork_unavailable"}));
        }
        if !snapshot::valid_playnite_id(&game_id)
            || !snapshot::valid_artwork_revision(&revision)
            || !connector::valid_artwork_transfer_id(&transfer_id)
            || offset >= 16 * 1024 * 1024
            || !offset.is_multiple_of(8192)
        {
            return self.complete(request, json!({"ok":false,"code":"invalid_cursor"}));
        }
        if !artwork_revision_may_be_read(
            &self.artwork_revisions,
            self.artwork_snapshot_current,
            catalog_reference,
            &game_id,
            &revision,
        ) {
            return self.complete(request, json!({"ok":false,"code":"artwork_changed"}));
        }
        if self.pending_artwork.len() >= 8 {
            return self.complete(request, json!({"ok":false,"code":"artwork_busy"}));
        }
        match connector::request_artwork_chunk(
            &self.last_discovery.processes,
            &game_id,
            &revision,
            offset,
            &transfer_id,
            !self.artwork_snapshot_current
                || self
                    .artwork_revisions
                    .get(&game_id)
                    .is_some_and(String::is_empty),
        ) {
            Ok(receiver) => self.pending_artwork.push(PendingArtwork {
                request,
                game_id,
                revision,
                transfer_id,
                catalog_reference,
                processes: self.last_discovery.processes.clone(),
                settings_epoch: self.settings_epoch,
                receiver,
            }),
            Err(_) => {
                return self.complete(request, json!({"ok":false,"code":"artwork_unavailable"}));
            }
        }
        Ok(())
    }

    fn poll_binary_releases(&mut self) {
        for (token, mut release) in std::mem::take(&mut self.binary_releases) {
            if Instant::now() < release.retry_at {
                self.binary_releases.insert(token, release);
                continue;
            }
            if let Some(receiver) = &release.receiver {
                match receiver.try_recv() {
                    Ok(Ok(())) => continue,
                    Err(mpsc::TryRecvError::Empty) => {
                        self.binary_releases.insert(token, release);
                        continue;
                    }
                    _ => release.receiver = None,
                }
            }
            if release.receiver.is_none() {
                release.receiver =
                    connector::release_artwork_transfer(&release.processes, &token).ok();
                release.retry_at = Instant::now() + Duration::from_secs(1);
            }
            self.binary_releases.insert(token, release);
        }
    }
    fn accept_binary_artwork(
        &mut self,
        item: PendingArtwork,
        chunk: Option<connector::ArtworkChunk>,
        current: bool,
    ) -> ModuleResult<()> {
        use base64::Engine;
        use sha2::{Digest, Sha256};
        let mut transfer = self.binary_requests.remove(&item.request).unwrap();
        let result = (|| -> Result<bool, &'static str> {
            if !current || Instant::now() >= transfer.deadline {
                return Err("artwork_changed");
            }
            let chunk = chunk.ok_or("artwork_unavailable")?;
            if chunk.total_bytes == 0
                || chunk.total_bytes > amaterasu_module_sdk::artwork_transfer::MAX_BYTES
                || chunk.offset >= chunk.total_bytes
                || chunk.offset != transfer.bytes.len()
                || transfer.total.is_some_and(|n| n != chunk.total_bytes)
                || transfer
                    .mime
                    .as_ref()
                    .is_some_and(|m| m != &chunk.mime_type)
                || !matches!(
                    chunk.mime_type.as_str(),
                    "image/png" | "image/jpeg" | "image/webp"
                )
                || chunk.data_base64.len() > 10924
            {
                return Err("invalid_artwork_response");
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(chunk.data_base64)
                .map_err(|_| "invalid_artwork_response")?;
            let expected = (chunk.total_bytes - chunk.offset).min(8192);
            if bytes.len() != expected {
                return Err("invalid_artwork_response");
            }
            transfer.total = Some(chunk.total_bytes);
            transfer.mime = Some(chunk.mime_type);
            transfer.bytes.extend(bytes);
            let complete = transfer.bytes.len() == chunk.total_bytes;
            if complete && format!("{:x}", Sha256::digest(&transfer.bytes)) != item.revision {
                return Err("artwork_changed");
            }
            Ok(complete)
        })();
        if matches!(result, Ok(false)) {
            let offset = transfer.bytes.len();
            self.binary_requests.insert(item.request, transfer);
            self.request_artwork(
                item.request,
                item.game_id,
                item.revision,
                offset,
                item.transfer_id.clone(),
                item.catalog_reference,
            )?;
            if self
                .pending_artwork
                .iter()
                .any(|next| next.request == item.request)
            {
                return Ok(());
            }
            self.binary_requests.remove(&item.request);
            self.binary_releases.insert(
                item.transfer_id,
                BinaryRelease {
                    processes: item.processes,
                    receiver: None,
                    retry_at: Instant::now(),
                },
            );
            return Ok(());
        }
        self.binary_releases.insert(
            item.transfer_id,
            BinaryRelease {
                processes: item.processes,
                receiver: None,
                retry_at: Instant::now(),
            },
        );
        let offer = result.and_then(|_| {
            self.binary_artwork.publish(
                &transfer.owner,
                item.revision,
                transfer.mime.unwrap(),
                transfer.bytes,
            )
        });
        self.complete(
            item.request,
            match offer {
                Ok(offer) => json!({"ok":true,"offer":offer}),
                Err(code) => json!({"ok":false,"code":code}),
            },
        )
    }

    fn poll_artwork(&mut self) -> ModuleResult<()> {
        let mut pending = std::mem::take(&mut self.pending_artwork);
        for item in pending.drain(..) {
            let result = item.receiver.try_recv();
            if matches!(result, Err(mpsc::TryRecvError::Empty)) {
                self.pending_artwork.push(item);
                continue;
            }
            let current = self.configuration.ready()
                && self.configuration.settings().enabled
                && item.settings_epoch == self.settings_epoch
                && item.processes == self.last_discovery.processes
                && artwork_revision_may_be_read(
                    &self.artwork_revisions,
                    self.artwork_snapshot_current,
                    item.catalog_reference,
                    &item.game_id,
                    &item.revision,
                );
            if self.binary_requests.contains_key(&item.request) {
                self.accept_binary_artwork(item, result.ok().and_then(Result::ok), current)?;
                continue;
            }
            let payload = match result {
                Ok(Ok(chunk)) if current => {
                    let next = (chunk.offset + 8192).min(chunk.total_bytes);
                    json!({"ok":true,"game_id":item.game_id,"revision":item.revision,"transfer_id":item.transfer_id,"offset":chunk.offset,
                        "next_offset":(next < chunk.total_bytes).then_some(next),"total_bytes":chunk.total_bytes,
                        "mime_type":chunk.mime_type,"data":chunk.data_base64})
                }
                _ => json!({"ok":false,"code":"artwork_changed"}),
            };
            self.complete(item.request, payload)?;
        }
        let releases = std::mem::take(&mut self.pending_artwork_releases);
        for release in releases {
            match release.receiver.try_recv() {
                Err(mpsc::TryRecvError::Empty) => self.pending_artwork_releases.push(release),
                Ok(Ok(())) => self.complete(
                    release.request,
                    json!({"ok":true,"transfer_id":release.transfer_id}),
                )?,
                _ => self.complete(
                    release.request,
                    json!({"ok":false,"code":"artwork_unavailable"}),
                )?,
            }
        }
        Ok(())
    }

    fn status(&self) -> Value {
        json!({
            "ok":true,
            "settings_ready":self.configuration.ready(),
            "enabled":self.configuration.settings().enabled,
            "catalog_epoch_ready":self.catalog.epoch_ready(),
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

fn library_principal_from_settings(settings: &Value) -> ModuleResult<String> {
    let object = settings
        .as_object()
        .ok_or("invalid Playnite startup settings")?;
    let principal = match object.get("library_principal") {
        None => LIBRARY_CALLER,
        Some(value) => value.as_str().ok_or("invalid library_principal")?,
    };
    if principal.is_empty()
        || principal.len() > 128
        || !principal
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || matches!(
            principal,
            "amaterasu.nvhttp" | MANAGEMENT_CALLER | LAUNCH_CALLER | "amaterasu.catalog"
        )
    {
        return Err("invalid library_principal".into());
    }
    Ok(principal.to_owned())
}

fn artwork_caller_allowed(caller: &str, library_principal: &str, artwork_read: bool) -> bool {
    caller != "amaterasu.nvhttp"
        && ((caller != library_principal && caller != LIBRARY_CALLER) || artwork_read)
        && (!artwork_read || caller == MANAGEMENT_CALLER || caller == library_principal)
}

fn artwork_revision_may_be_read(
    current: &BTreeMap<String, String>,
    snapshot_current: bool,
    catalog_reference: bool,
    id: &str,
    revision: &str,
) -> bool {
    (!snapshot_current && catalog_reference)
        || current
            .get(id)
            .is_some_and(|known| (known.is_empty() && catalog_reference) || known == revision)
}

fn update_artwork_revisions(current: &mut BTreeMap<String, String>, snapshot: &Snapshot) {
    let mut next = BTreeMap::new();
    for game in &snapshot.games {
        let id = game.id.to_ascii_lowercase();
        if !snapshot::valid_playnite_id(&id) {
            continue;
        }
        let revision = match game.artwork_state {
            // An empty value marks a game in the current complete snapshot
            // whose artwork scan is unknown. The authenticated connector must
            // still hash the opened image and match the requested revision.
            snapshot::ArtworkState::Unknown => Some(String::new()),
            snapshot::ArtworkState::Missing => None,
            snapshot::ArtworkState::Revision
                if snapshot::valid_artwork_revision(&game.artwork_revision) =>
            {
                Some(game.artwork_revision.clone())
            }
            snapshot::ArtworkState::Revision => None,
        };
        if let Some(revision) = revision {
            next.insert(id, revision);
        }
    }
    *current = next;
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

fn update_discovery_state(
    previous: &[discovery::ProcessImage],
    current: &[discovery::ProcessImage],
    eligible_launch_ids: &mut std::collections::BTreeSet<String>,
    next_auto_sync: &mut Option<Instant>,
    auto_sync_enabled: bool,
    now: Instant,
) -> bool {
    if previous != current {
        eligible_launch_ids.clear();
        if auto_sync_enabled && !current.is_empty() {
            *next_auto_sync = Some(now);
        }
        true
    } else {
        false
    }
}

fn apply_settings_transition(
    previous: Option<&ProductSettings>,
    current: &ProductSettings,
    eligible_launch_ids: &mut std::collections::BTreeSet<String>,
    catalog_processes: &mut Option<Vec<discovery::ProcessImage>>,
    next_auto_sync: &mut Option<Instant>,
    now: Instant,
) -> bool {
    let same_catalog_policy = previous.is_some_and(|previous| {
        previous.enabled
            && current.enabled
            && previous.auto_sync == current.auto_sync
            && (!current.auto_sync
                || (previous.sync_all_installed == current.sync_all_installed
                    && previous.autosync_remove_uninstalled == current.autosync_remove_uninstalled
                    && previous.autosync_delete_after_days == current.autosync_delete_after_days
                    && previous.autosync_require_replacement
                        == current.autosync_require_replacement
                    && previous.recent_games == current.recent_games
                    && previous.recent_max_age_days == current.recent_max_age_days
                    && previous.sync_categories == current.sync_categories
                    && previous.exclude_categories == current.exclude_categories
                    && previous.sync_plugins == current.sync_plugins
                    && previous.exclude_plugins == current.exclude_plugins
                    && previous.exclude_games == current.exclude_games))
    });
    if !same_catalog_policy {
        eligible_launch_ids.clear();
        *catalog_processes = None;
    }
    if !current.enabled || !current.auto_sync {
        *next_auto_sync = None;
    } else if !same_catalog_policy {
        *next_auto_sync = Some(now);
    }
    same_catalog_policy
}

fn can_publish_eligibility(
    catalog_processes: Option<&[discovery::ProcessImage]>,
    discovery: &discovery::Discovery,
) -> bool {
    discovery.running && catalog_processes == Some(discovery.processes.as_slice())
}

fn snapshot_processes_match_instance(
    snapshot_processes: &[discovery::ProcessImage],
    discovery: &discovery::Discovery,
) -> bool {
    discovery.running && snapshot_processes == discovery.processes
}

fn unsolicited_snapshot_is_current(
    event: &connector::SnapshotEvent,
    settings: &ProductSettings,
    settings_epoch: u64,
    discovery: &discovery::Discovery,
) -> bool {
    settings.enabled
        && settings.auto_sync
        && event.settings_epoch == settings_epoch
        && snapshot_processes_match_instance(&event.processes, discovery)
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
