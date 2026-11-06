use crate::STATE_SERVICE;
use crate::model::{
    ArtworkReference, ConfigOverrides, Document, Entry, Launch, MAX_ENTRIES, bounded_text,
};
use crate::product::{
    CommittedBatch, ENTRY_PREFIX, MAX_BATCH_BYTES, MAX_BATCH_ENTRIES, Marker, RemovalReason,
    STATE_PREFIX, SnapshotState, StagedIntents, entry_key, fingerprint_bytes, marker_value,
    parse_entry_key, provider_entry_fingerprint, state_key, valid_source_id, valid_sync_id,
    validate_marker,
};
use crate::protocol::{
    Change, Cursor, Operation, ProviderEntry, apply, error, matches_query, project_entry,
};
use crate::seed::{self, DesktopSeed};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
mod artwork_refresh;

const MAX_QUEUED: usize = 32;
const PAGE_BYTES: usize = 12_000;
const STAGED_PREFIX: &str = "~provider.batch:";
const MAX_SCAN: usize = 256;
// A launch scan can cross public entries and their provider markers before
// reaching the Lutris prefix, including unpublished staged-batch rows.
const MAX_LAUNCH_MARKER_SCAN: usize = 100_000;
const MANAGEMENT_PRINCIPAL: &str = "amaterasu.management";
const LIBRARY_PRINCIPAL: &str = "amaterasu.library";
const PLAYNITE_IMPORTER: &str = "amaterasu.playnite";
const STEAM_IMPORTER: &str = "amaterasu.steam";
const LUTRIS_IMPORTER: &str = "amaterasu.lutris";
const PLAYNITE_FULLSCREEN_SOURCE_ID: &str = "fullscreen-entry";
const PLAYNITE_FULLSCREEN_NAME: &str = "Playnite (Fullscreen)";
type TransportResult = ModuleResult<Response>;

struct Incoming {
    request_id: u64,
    caller: String,
    key: String,
    catalog_owner: Option<String>,
    operation: Operation,
    deadline: Instant,
}
struct Context {
    request_id: u64,
    key: String,
    catalog_owner: Option<String>,
    importer: Option<String>,
    provider_count: u64,
    deadline: Instant,
}
struct Pending {
    context: Context,
    state_request_id: u64,
    stage: Stage,
}
enum Stage {
    ArtworkMarker {
        source_id: String,
        artwork: ArtworkReference,
    },
    ArtworkEntry(Box<artwork_refresh::Refresh>),
    Loading(Operation),
    LaunchMarkerPage(LaunchMarkerScan),
    Legacy(Operation),
    Migrating(Operation),
    Paging(Scan),
    Saving(Value),
    ProviderLoading(Operation),
    ProviderInventoryCheck {
        operation: Operation,
        session: ProviderSession,
    },
    ProviderFullscreenEntryRow(ProviderFullscreenEntry),
    ProviderMarkerCheck {
        operation: Operation,
        state_revision: u64,
        document: Document,
        id: String,
    },
    ProviderPaging(ProviderScan),
    ProviderSaving(ProviderSave),
    StagedCacheCheck {
        operation: Operation,
        session: ProviderSession,
    },
    DraftReset(DraftPublish),
    DraftBegin(DraftPublish),
    DraftAppend(DraftPublish),
    DraftVerify(DraftPublish),
    DraftPublishing(DraftPublish),
    ProviderOverrideEntry(Box<ProviderOverrideEntry>),
}
struct LaunchMarkerScan {
    reply: Value,
    state_revision: u64,
    entry_id: String,
    fingerprint: String,
    importer: String,
    expected_source_id: Option<String>,
    after: Option<String>,
    scanned: usize,
    source_id: Option<String>,
    matched: bool,
}

struct ProviderOverrideEntry {
    session: ProviderSession,
    id: String,
    expected_revision: u64,
    config_overrides: ConfigOverrides,
    marker_key: String,
    marker: Marker,
}

struct ProviderFullscreenEntry {
    session: ProviderSession,
    enabled: bool,
    marker_key: String,
    marker: Marker,
}

struct DraftPublish {
    save: ProviderSave,
    draft_id: String,
    content_id: String,
    chunks: Vec<Vec<Value>>,
    next: usize,
}

struct ProviderScan {
    operation: Operation,
    state_revision: u64,
    document_revision: u64,
    next_id: u64,
    total_count: u64,
    provider_count: u64,
    hidden_rows: u64,
    after: Option<String>,
    scanned: usize,
    fingerprints: BTreeMap<String, String>,
    entry_names: BTreeMap<String, String>,
    artwork_entries: BTreeMap<String, Entry>,
    entry_ids: BTreeSet<String>,
    markers: BTreeMap<String, Marker>,
    marker_entry_ids: BTreeSet<String>,
    provider_state: Option<SnapshotState>,
    staged_rows: BTreeMap<String, StagedIntents>,
}

struct ProviderInventoryRequest {
    after: Option<String>,
    expected_collection_revision: Option<u64>,
    limit: usize,
    include_entry_names: bool,
}

#[derive(Clone)]
struct ProviderSession {
    caller: String,
    provider_key: Option<String>,
    key: String,
    state_present: bool,
    collection_revision: u64,
    document_revision: u64,
    next_id: u64,
    provider_count: u64,
    entry_ids: BTreeSet<String>,
    fingerprints: BTreeMap<String, String>,
    entry_names: BTreeMap<String, String>,
    artwork_entries: BTreeMap<String, Entry>,
    markers: BTreeMap<String, Marker>,
    seen_source_ids: BTreeSet<String>,
    state: SnapshotState,
    staged_rows: BTreeMap<String, StagedIntents>,
}

struct ProviderSave {
    session: ProviderSession,
    reply: Value,
    remaining_cleanup: Vec<Marker>,
    finish: bool,
}
struct ProviderBatch {
    sequence: u64,
    expected_revision: u64,
    fingerprint: String,
    entries: Vec<ProviderEntry>,
    remove_source_ids: Vec<String>,
    expire_source_ids: Vec<String>,
    selection_remove_source_ids: Vec<String>,
}
struct ProviderBegin {
    staged: bool,
    sync_id: String,
    source_revision: u64,
    provider_epoch: u64,
    expected_collection_revision: Option<u64>,
}
struct ProviderFinish {
    sync_id: String,
    generation: u64,
    source_revision: u64,
    provider_epoch: u64,
    expected_revision: u64,
    next_sequence: u64,
    source_complete: bool,
    remove_stale: bool,
    remove_uninstalled: bool,
    remove_expired: bool,
    remove_selection: bool,
}
#[derive(Clone, Copy)]
enum ScanProjection {
    List,
    ExportList,
    IdentityInventory,
    Profiles,
    SteamIds,
}

struct Scan {
    projection: ScanProjection,
    state_revision: u64,
    revision: u64,
    query: String,
    cursor_query: String,
    limit: usize,
    entries: Vec<Value>,
    bytes: usize,
    scanned: usize,
    after: Option<String>,
}
#[derive(Default)]
pub struct Pipeline {
    library_principal: Option<String>,
    launch_principals: BTreeSet<String>,
    export_principals: BTreeMap<String, String>,
    profile_principals: BTreeMap<String, String>,
    steam_id_export_principals: BTreeMap<String, String>,
    import_principals: BTreeMap<String, String>,
    provider_launch_principals: BTreeMap<String, String>,
    provider_sessions: BTreeMap<String, ProviderSession>,
    queued: VecDeque<Incoming>,
    pending: Option<Pending>,
    sequence: u64,
    seed: DesktopSeed,
}
impl Pipeline {
    pub fn with_profile_principals(mut self, principals: BTreeMap<String, String>) -> Self {
        self.profile_principals = principals;
        self
    }
    pub fn with_library_principal(mut self, principal: String) -> Self {
        self.library_principal = Some(principal);
        self
    }

    pub fn new_with_steam_id_export_principals(
        launch_principals: BTreeSet<String>,
        export_principals: BTreeMap<String, String>,
        steam_id_export_principals: BTreeMap<String, String>,
        import_principals: BTreeMap<String, String>,
    ) -> Self {
        Self {
            launch_principals,
            export_principals,
            steam_id_export_principals,
            import_principals,
            ..Self::default()
        }
    }

    pub fn with_provider_launch_principals(mut self, principals: BTreeMap<String, String>) -> Self {
        self.provider_launch_principals = principals;
        self
    }

    /// Seed the Desktop entry once into each owner's catalog before ordinary
    /// requests are served. See [`crate::seed`] for the durable marker.
    pub fn with_desktop_seed(mut self, owners: impl IntoIterator<Item = String>) -> Self {
        self.seed = DesktopSeed::new(owners.into_iter().map(|owner| format!("principal/{owner}")));
        self
    }

    fn select_catalog(
        &self,
        caller: &str,
        initiator: &str,
        operation: Operation,
    ) -> Result<(String, Option<String>, Operation), Value> {
        let library_principal = self
            .library_principal
            .as_deref()
            .unwrap_or(LIBRARY_PRINCIPAL);
        if matches!(operation, Operation::ProviderPlayniteFullscreenEntry { .. })
            && caller != PLAYNITE_IMPORTER
        {
            return Err(error(
                "permission_denied",
                "Playnite Fullscreen entry is reserved for the fixed Playnite importer",
            ));
        }
        match operation {
            operation @ (Operation::ProfileList { .. }
            | Operation::ProfileUpsert { .. }
            | Operation::ProfileDelete { .. }) => {
                if initiator != MANAGEMENT_PRINCIPAL {
                    return Err(error(
                        "permission_denied",
                        "profile mutations require the management initiator",
                    ));
                }
                let owner = self.profile_principals.get(caller).ok_or_else(|| {
                    error(
                        "permission_denied",
                        "caller is not an authorized profile coordinator",
                    )
                })?;
                Ok((format!("principal/{owner}"), None, operation))
            }
            operation @ (Operation::ProviderSnapshotBegin { .. }
            | Operation::ProviderArtworkRefresh { .. }
            | Operation::ProviderSnapshotRevoke { .. }
            | Operation::ProviderSnapshotInventory { .. }
            | Operation::ProviderSnapshotBatch { .. }
            | Operation::ProviderSnapshotFinish { .. }
            | Operation::ProviderPlayniteFullscreenEntry { .. }) => {
                let owner = self.import_principals.get(caller).ok_or_else(|| {
                    error(
                        "permission_denied",
                        "caller is not an authorized catalog importer",
                    )
                })?;
                Ok((format!("principal/{owner}"), None, operation))
            }
            operation @ Operation::UpdateProviderOverrides { .. } => {
                if caller != MANAGEMENT_PRINCIPAL {
                    return Err(error(
                        "permission_denied",
                        "provider overrides can only be changed by management",
                    ));
                }
                Ok((format!("principal/{caller}"), None, operation))
            }
            operation @ Operation::ExportIdentityInventory { .. } => {
                if caller != library_principal {
                    return Err(error(
                        "permission_denied",
                        "caller is not the library facade",
                    ));
                }
                let owner = self.export_principals.get(initiator).ok_or_else(|| {
                    error(
                        "permission_denied",
                        "request initiator is not an authorized catalog consumer",
                    )
                })?;
                Ok((format!("principal/{owner}"), None, operation))
            }
            operation @ Operation::ExportList { .. } => {
                let consumer = if caller == library_principal {
                    initiator
                } else {
                    caller
                };
                let owner = self.export_principals.get(consumer).ok_or_else(|| {
                    error(
                        "permission_denied",
                        "caller is not an authorized catalog consumer",
                    )
                })?;
                Ok((format!("principal/{owner}"), None, operation))
            }
            operation @ Operation::ResolveExported { .. } => {
                if caller != library_principal {
                    return Err(error(
                        "permission_denied",
                        "caller is not the library facade",
                    ));
                }
                let owner = self.export_principals.get(initiator).ok_or_else(|| {
                    error(
                        "permission_denied",
                        "request initiator is not an authorized catalog consumer",
                    )
                })?;
                Ok((format!("principal/{owner}"), None, operation))
            }
            operation @ Operation::ResolveExportedArtwork { .. } => {
                let consumer = if caller == library_principal {
                    initiator
                } else {
                    caller
                };
                let owner = self.export_principals.get(consumer).ok_or_else(|| {
                    error(
                        "permission_denied",
                        "caller is not an authorized catalog consumer",
                    )
                })?;
                Ok((format!("principal/{owner}"), None, operation))
            }
            operation @ Operation::ExportSteamIds { .. } => {
                let owner = self.steam_id_export_principals.get(caller).ok_or_else(|| {
                    error(
                        "permission_denied",
                        "caller is not authorized for Steam-ID catalog export",
                    )
                })?;
                Ok((format!("principal/{owner}"), None, operation))
            }
            Operation::ResolveLaunch { id } => {
                if !self.launch_principals.contains(caller) {
                    return Err(error(
                        "permission_denied",
                        "caller is not an authorized launch provider",
                    ));
                }
                // The runtime binds initiator across parented delegation. The payload
                // cannot select an owner or widen this provider's access to another catalog.
                Ok((
                    format!("principal/{initiator}"),
                    None,
                    Operation::ResolveLaunch { id },
                ))
            }
            operation @ Operation::ResolveExportedLaunch { .. } => {
                if !self.launch_principals.contains(caller) {
                    return Err(error(
                        "permission_denied",
                        "caller is not an authorized launch provider",
                    ));
                }
                let owner = self.export_principals.get(initiator).ok_or_else(|| {
                    error(
                        "permission_denied",
                        "request initiator is not an authorized catalog consumer",
                    )
                })?;
                Ok((format!("principal/{owner}"), Some(owner.clone()), operation))
            }
            operation => Ok((format!("principal/{caller}"), None, operation)),
        }
    }

    pub fn tick(&mut self, mut send: impl FnMut(Command) -> TransportResult) -> ModuleResult<()> {
        if let Some(pending) = self.pending.take() {
            self.advance(pending, &mut send)?;
        }
        if self.queued.len() < MAX_QUEUED {
            self.receive(&mut send)?;
        }
        if self.pending.is_none() && self.seed.needs_turn() {
            return self.seed.step(&mut send);
        }
        if self.pending.is_none()
            && let Some(incoming) = self.queued.pop_front()
        {
            let context = Context {
                request_id: incoming.request_id,
                key: incoming.key,
                catalog_owner: incoming.catalog_owner,
                importer: match &incoming.operation {
                    Operation::ProviderSnapshotBegin { .. }
                    | Operation::ProviderArtworkRefresh { .. }
                    | Operation::ProviderSnapshotRevoke { .. }
                    | Operation::ProviderSnapshotInventory { .. }
                    | Operation::ProviderSnapshotBatch { .. }
                    | Operation::ProviderSnapshotFinish { .. } => Some(incoming.caller.clone()),
                    Operation::ProviderPlayniteFullscreenEntry { .. } => {
                        Some(incoming.caller.clone())
                    }
                    Operation::UpdateProviderOverrides { .. } => Some(PLAYNITE_IMPORTER.into()),
                    _ => None,
                },
                provider_count: 0,
                deadline: incoming.deadline,
            };
            // Snapshot authority comes from the Poll caller. Provider override
            // requests use a fixed importer only after management authorization;
            // neither path accepts an owner selector in request data.
            if let Operation::ProviderArtworkRefresh { source_id, artwork } = incoming.operation {
                self.start_artwork_refresh(context, source_id, artwork, &mut send)?;
            } else if matches!(
                incoming.operation,
                Operation::ProviderSnapshotBegin { .. }
                    | Operation::ProviderSnapshotRevoke { .. }
                    | Operation::ProviderSnapshotInventory { .. }
                    | Operation::ProviderSnapshotBatch { .. }
                    | Operation::ProviderSnapshotFinish { .. }
                    | Operation::ProviderPlayniteFullscreenEntry { .. }
                    | Operation::UpdateProviderOverrides { .. }
            ) {
                self.start_provider(context, incoming.operation, &mut send)?;
            } else {
                self.load(context, incoming.operation, &mut send)?;
            }
        }
        Ok(())
    }
    fn receive(&mut self, send: &mut impl FnMut(Command) -> TransportResult) -> ModuleResult<()> {
        match send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                initiator,
                payload,
                remaining_ms,
                ..
            } => {
                let operation = match serde_json::from_value::<Operation>(payload) {
                    Ok(operation) => operation,
                    Err(failure) => {
                        return complete(send, id, error("invalid_request", failure.to_string()));
                    }
                };
                let (key, catalog_owner, operation) =
                    match self.select_catalog(&caller, &initiator, operation) {
                        Ok(selected) => selected,
                        Err(failure) => return complete(send, id, failure),
                    };
                let incoming = Incoming {
                    request_id: id,
                    caller,
                    key,
                    catalog_owner,
                    operation,
                    deadline: Instant::now() + Duration::from_millis(remaining_ms.min(30_000)),
                };
                if matches!(incoming.operation, Operation::ProviderSnapshotRevoke { .. }) {
                    // Once the current State request settles, fence queued snapshot work
                    // before it can start another catalog mutation.
                    let position = self
                        .queued
                        .iter()
                        .position(|queued| {
                            !matches!(queued.operation, Operation::ProviderSnapshotRevoke { .. })
                        })
                        .unwrap_or(self.queued.len());
                    self.queued.insert(position, incoming);
                } else {
                    self.queued.push_back(incoming);
                }
                Ok(())
            }
            unexpected => Err(format!("catalog poll failed: {unexpected:?}")),
        }
    }
    fn load(
        &mut self,
        context: Context,
        operation: Operation,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let key = match &operation {
            Operation::Get { id }
            | Operation::ResolveLaunch { id }
            | Operation::ResolveExported { id }
            | Operation::ResolveExportedArtwork { id }
            | Operation::ResolveExportedLaunch { id }
            | Operation::Update { id, .. }
            | Operation::Delete { id, .. } => Some(id),
            Operation::ProfileUpsert { id, .. } if !id.is_empty() => Some(id),
            Operation::ProfileDelete { id, .. } => Some(id),
            _ => None,
        };
        let payload = json!({"action":"collection_get","collection":context.key,"key":key});
        self.submit(context, payload, Stage::Loading(operation), send)
    }

    fn start_provider(
        &mut self,
        context: Context,
        operation: Operation,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let Some(caller) = context.importer.as_deref() else {
            return complete(
                send,
                context.request_id,
                error(
                    "permission_denied",
                    "provider caller identity is unavailable",
                ),
            );
        };
        if matches!(operation, Operation::ProviderSnapshotBatch { .. })
            && self
                .provider_sessions
                .get(caller)
                .is_some_and(|session| session.state.staged)
            && let Some(session) = self.provider_sessions.remove(caller)
        {
            let payload = json!({"action":"collection_get","collection":context.key,"key":null});
            return self.submit(
                context,
                payload,
                Stage::StagedCacheCheck { operation, session },
                send,
            );
        }
        if !matches!(
            operation,
            Operation::ProviderSnapshotBegin { .. } | Operation::ProviderSnapshotRevoke { .. }
        ) && let Operation::ProviderSnapshotInventory { after: Some(_), .. } = &operation
            && let Some(session) = self.provider_sessions.get(caller).cloned()
        {
            let payload = json!({"action":"collection_get","collection":context.key,"key":null});
            return self.submit(
                context,
                payload,
                Stage::ProviderInventoryCheck { operation, session },
                send,
            );
        }
        if !matches!(
            operation,
            Operation::ProviderSnapshotBegin { .. } | Operation::ProviderSnapshotRevoke { .. }
        ) && !matches!(operation, Operation::ProviderSnapshotInventory { .. })
            && matches!(operation, Operation::ProviderPlayniteFullscreenEntry { .. })
            && let Some(session) = self.provider_sessions.get(caller).cloned()
        {
            let payload = json!({"action":"collection_get","collection":context.key,"key":null});
            return self.submit(
                context,
                payload,
                Stage::ProviderInventoryCheck { operation, session },
                send,
            );
        }
        if !matches!(
            operation,
            Operation::ProviderSnapshotBegin { .. } | Operation::ProviderSnapshotRevoke { .. }
        ) && !matches!(operation, Operation::ProviderSnapshotInventory { .. })
            && !(matches!(
                operation,
                Operation::ProviderSnapshotBatch { .. } | Operation::ProviderSnapshotFinish { .. }
            ) && (caller == PLAYNITE_IMPORTER
                || self
                    .provider_sessions
                    .get(caller)
                    .is_some_and(|session| session.state.staged)))
            && let Some(session) = self.provider_sessions.get(caller).cloned()
        {
            return self.apply_provider_operation(context, operation, session, send);
        }
        let payload = json!({"action":"collection_get","collection":context.key,"key":null});
        self.submit(context, payload, Stage::ProviderLoading(operation), send)
    }

    fn scan_provider_page(
        &mut self,
        context: Context,
        scan: ProviderScan,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let payload = json!({
            "action":"collection_page",
            "collection":context.key,
            "revision":scan.state_revision,
            "after":scan.after,
            "limit":128,
        });
        self.submit(context, payload, Stage::ProviderPaging(scan), send)
    }

    fn finish_provider_scan(
        &mut self,
        context: Context,
        scan: ProviderScan,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let caller = context.importer.clone().expect("authorized importer");
        let state_present = scan.provider_state.is_some();
        let state = scan.provider_state.unwrap_or_else(SnapshotState::initial);
        if !state.validate() {
            return complete(send, context.request_id, malformed());
        }
        let seen_source_ids = scan
            .markers
            .values()
            .filter(|marker| {
                marker.caller == caller
                    && (marker.generation == state.generation
                        || marker.remove_generation == Some(state.generation))
            })
            .map(|marker| marker.source_id.clone())
            .collect();
        let mut session = ProviderSession {
            provider_key: self.provider_launch_principals.get(&caller).cloned(),
            caller,
            key: context.key.clone(),
            state_present,
            collection_revision: scan.state_revision,
            document_revision: scan.document_revision,
            next_id: scan.next_id,
            provider_count: scan.provider_count,
            entry_ids: scan.entry_ids,
            fingerprints: scan.fingerprints,
            entry_names: scan.entry_names,
            artwork_entries: scan.artwork_entries,
            markers: scan.markers,
            seen_source_ids,
            state,
            staged_rows: scan.staged_rows,
        };
        if session.state.staged && !session.state.finished {
            let intents = match collect_staged(&session) {
                Ok(intents) => intents,
                Err(failure) => return complete(send, context.request_id, failure),
            };
            session.seen_source_ids = match validate_staged_intents(
                &session.caller,
                session.provider_key.as_deref(),
                &BTreeSet::new(),
                &intents,
            ) {
                Ok(ids) => ids,
                Err(failure) => return complete(send, context.request_id, failure),
            };
        }
        self.apply_provider_operation(context, scan.operation, session, send)
    }

    fn provider_inventory_page(
        &mut self,
        context: Context,
        session: ProviderSession,
        request: ProviderInventoryRequest,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if !(1..=32).contains(&request.limit)
            || request
                .after
                .as_ref()
                .is_some_and(|value| !valid_source_id(value))
            || request.after.is_some() != request.expected_collection_revision.is_some()
        {
            return complete(
                send,
                context.request_id,
                error("invalid_request", "provider inventory cursor is invalid"),
            );
        }
        if let Some(expected) = request.expected_collection_revision
            && expected != session.collection_revision
        {
            return complete(
                send,
                context.request_id,
                json!({"ok":false,"error":{"code":"revision_conflict","message":"catalog changed between inventory pages","current_collection_revision":session.collection_revision}}),
            );
        }

        let after = request.after.as_deref().unwrap_or_default();
        let mut markers: Vec<_> = session
            .markers
            .values()
            .filter(|marker| marker.caller == session.caller && marker.source_id.as_str() > after)
            .collect();
        markers.sort_by(|left, right| left.source_id.cmp(&right.source_id));
        let total_count = markers.len();
        let has_more = markers.len() > request.limit;
        let entries: Vec<_> = markers
            .iter()
            .take(request.limit)
            .map(|marker| {
                let attached = !marker.detached
                    && session.fingerprints.get(&marker.entry_id) == Some(&marker.fingerprint);
                let mut row = json!({
                    "source_id":marker.source_id,
                    "added_at_unix":marker.added_at_unix,
                    "source_flags":marker.source_flags,
                    "attached":attached,
                });
                if request.include_entry_names {
                    row["entry_name"] = if attached {
                        session
                            .entry_names
                            .get(&marker.entry_id)
                            .cloned()
                            .map(Value::String)
                            .unwrap_or(Value::Null)
                    } else {
                        Value::Null
                    };
                }
                row
            })
            .collect();
        let next_after = if has_more {
            entries.last().and_then(|entry| entry["source_id"].as_str())
        } else {
            None
        };
        self.provider_sessions
            .insert(session.caller.clone(), session.clone());
        complete(
            send,
            context.request_id,
            json!({
                "ok":true,
                "collection_revision":session.collection_revision,
                "provider_epoch":session.state.provider_epoch,
                "total_count":total_count,
                "entries":entries,
                "next_after":next_after,
            }),
        )
    }

    fn apply_provider_operation(
        &mut self,
        context: Context,
        operation: Operation,
        session: ProviderSession,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        match operation {
            Operation::ProviderSnapshotBegin {
                sync_id,
                source_revision,
                provider_epoch,
                expected_collection_revision,
                staged,
            } => self.begin_snapshot(
                context,
                session,
                ProviderBegin {
                    staged,
                    sync_id,
                    source_revision,
                    provider_epoch,
                    expected_collection_revision,
                },
                send,
            ),
            Operation::ProviderSnapshotRevoke {
                revocation_id,
                expected_provider_epoch,
            } => self.revoke_snapshot(
                context,
                session,
                revocation_id,
                expected_provider_epoch,
                send,
            ),
            Operation::ProviderSnapshotInventory {
                after,
                expected_collection_revision,
                limit,
                include_entry_names,
            } => self.provider_inventory_page(
                context,
                session,
                ProviderInventoryRequest {
                    after,
                    expected_collection_revision,
                    limit: limit.unwrap_or(32),
                    include_entry_names,
                },
                send,
            ),
            Operation::ProviderSnapshotBatch {
                sync_id,
                generation,
                source_revision,
                provider_epoch,
                sequence,
                expected_revision,
                entries,
                remove_source_ids,
                expire_source_ids,
                selection_remove_source_ids,
            } => {
                if session.caller == PLAYNITE_IMPORTER
                    && provider_epoch != session.state.provider_epoch
                {
                    return complete(
                        send,
                        context.request_id,
                        error("provider_epoch_conflict", "Playnite provider epoch changed"),
                    );
                }
                if session.caller != PLAYNITE_IMPORTER && provider_epoch != 0 {
                    return complete(
                        send,
                        context.request_id,
                        error("invalid_request", "provider epoch is reserved for Playnite"),
                    );
                }
                let batch_payload = json!({
                    "entries":&entries,
                    "remove_source_ids":&remove_source_ids,
                    "expire_source_ids":&expire_source_ids,
                    "selection_remove_source_ids":&selection_remove_source_ids,
                });
                let encoded =
                    serde_json::to_vec(&batch_payload).map_or(usize::MAX, |bytes| bytes.len());
                let mut batch_source_ids = BTreeSet::new();
                let unique_sources = entries
                    .iter()
                    .all(|entry| batch_source_ids.insert(entry.source_id.as_str()))
                    && remove_source_ids
                        .iter()
                        .all(|source_id| batch_source_ids.insert(source_id.as_str()))
                    && expire_source_ids
                        .iter()
                        .all(|source_id| batch_source_ids.insert(source_id.as_str()))
                    && selection_remove_source_ids
                        .iter()
                        .all(|source_id| batch_source_ids.insert(source_id.as_str()));
                let intent_count = entries
                    .len()
                    .saturating_add(remove_source_ids.len())
                    .saturating_add(expire_source_ids.len())
                    .saturating_add(selection_remove_source_ids.len());
                if !valid_sync_id(&sync_id)
                    || intent_count == 0
                    || intent_count > MAX_BATCH_ENTRIES
                    || encoded > MAX_BATCH_BYTES
                    || !unique_sources
                {
                    return complete(
                        send,
                        context.request_id,
                        error("invalid_request", "provider batch exceeds its bounds"),
                    );
                }
                let batch_fingerprint = serde_json::to_vec(&batch_payload)
                    .map(|bytes| fingerprint_bytes(&bytes))
                    .unwrap_or_default();
                if !snapshot_matches(
                    &session,
                    &sync_id,
                    generation,
                    source_revision,
                    provider_epoch,
                ) {
                    return complete(
                        send,
                        context.request_id,
                        error("snapshot_conflict", "provider snapshot generation differs"),
                    );
                }
                if let Some(last) = &session.state.last_batch
                    && sequence == last.sequence
                {
                    if expected_revision == last.expected_revision
                        && batch_fingerprint == last.fingerprint
                    {
                        return complete(
                            send,
                            context.request_id,
                            json!({"ok":true,"revision":last.revision,"generation":generation,"provider_epoch":provider_epoch,"next_sequence":last.next_sequence,"replayed":true}),
                        );
                    }
                    return complete(
                        send,
                        context.request_id,
                        error("sequence_conflict", "replayed batch revision differs"),
                    );
                }
                if session.state.finished {
                    return complete(
                        send,
                        context.request_id,
                        error("snapshot_finished", "provider snapshot is already finished"),
                    );
                }
                if sequence != session.state.next_sequence {
                    return complete(
                        send,
                        context.request_id,
                        error("sequence_conflict", "provider batch sequence is not next"),
                    );
                }
                if expected_revision != session.document_revision {
                    return complete(
                        send,
                        context.request_id,
                        revision_conflict(session.document_revision),
                    );
                }
                if session.state.staged {
                    return self.stage_provider_batch(
                        context,
                        session,
                        ProviderBatch {
                            sequence,
                            expected_revision,
                            fingerprint: batch_fingerprint,
                            entries,
                            remove_source_ids,
                            expire_source_ids,
                            selection_remove_source_ids,
                        },
                        send,
                    );
                }
                self.commit_provider_batch(
                    context,
                    session,
                    ProviderBatch {
                        sequence,
                        expected_revision,
                        fingerprint: batch_fingerprint,
                        entries,
                        remove_source_ids,
                        expire_source_ids,
                        selection_remove_source_ids,
                    },
                    send,
                )
            }
            Operation::ProviderSnapshotFinish {
                sync_id,
                generation,
                source_revision,
                provider_epoch,
                expected_revision,
                next_sequence,
                complete: source_complete,
                remove_stale,
                remove_uninstalled,
                remove_expired,
                remove_selection,
            } => {
                if session.caller == PLAYNITE_IMPORTER
                    && provider_epoch != session.state.provider_epoch
                {
                    return complete(
                        send,
                        context.request_id,
                        error("provider_epoch_conflict", "Playnite provider epoch changed"),
                    );
                }
                if session.caller != PLAYNITE_IMPORTER && provider_epoch != 0 {
                    return complete(
                        send,
                        context.request_id,
                        error("invalid_request", "provider epoch is reserved for Playnite"),
                    );
                }
                self.finish_snapshot(
                    context,
                    session,
                    ProviderFinish {
                        sync_id,
                        generation,
                        source_revision,
                        provider_epoch,
                        expected_revision,
                        next_sequence,
                        source_complete,
                        remove_stale,
                        remove_uninstalled,
                        remove_expired,
                        remove_selection,
                    },
                    send,
                )
            }
            Operation::UpdateProviderOverrides {
                id,
                expected_revision,
                config_overrides,
            } => self.start_provider_override_update(
                context,
                session,
                id,
                expected_revision,
                config_overrides,
                send,
            ),
            Operation::ProviderPlayniteFullscreenEntry { enabled } => {
                self.reconcile_playnite_fullscreen_entry(context, session, enabled, send)
            }
            _ => complete(
                send,
                context.request_id,
                error(
                    "invalid_request",
                    "operation is not a provider snapshot action",
                ),
            ),
        }
    }

    fn start_provider_override_update(
        &mut self,
        context: Context,
        session: ProviderSession,
        id: String,
        expected_revision: u64,
        config_overrides: ConfigOverrides,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if session.caller != PLAYNITE_IMPORTER {
            return complete(
                send,
                context.request_id,
                error(
                    "permission_denied",
                    "only Playnite entries accept provider overrides",
                ),
            );
        }
        if crate::model::parse_id(&id).is_none() || config_overrides.validate().is_err() {
            return complete(
                send,
                context.request_id,
                error(
                    "invalid_request",
                    "provider overrides contain an invalid ID or config value",
                ),
            );
        }
        if !session.state.finished || session.state.cleanup_pending {
            return complete(
                send,
                context.request_id,
                error(
                    "provider_snapshot_incomplete",
                    "provider overrides require a finished Playnite snapshot",
                ),
            );
        }
        let Some((marker_key, marker)) = session
            .markers
            .iter()
            .find(|(_, marker)| marker.caller == PLAYNITE_IMPORTER && marker.entry_id == id)
        else {
            return complete(
                send,
                context.request_id,
                error(
                    "provider_entry_not_found",
                    "Playnite provider entry was not found",
                ),
            );
        };
        if marker.detached || session.fingerprints.get(&id) != Some(&marker.fingerprint) {
            return complete(
                send,
                context.request_id,
                error(
                    "provider_entry_changed",
                    "Playnite provider entry no longer matches its imported identity",
                ),
            );
        }
        let marker_key = marker_key.clone();
        let marker = marker.clone();
        let payload = json!({
            "action":"collection_get",
            "collection":session.key,
            "key":id,
        });
        self.submit(
            context,
            payload,
            Stage::ProviderOverrideEntry(Box::new(ProviderOverrideEntry {
                session,
                id,
                expected_revision,
                config_overrides,
                marker_key,
                marker,
            })),
            send,
        )
    }

    fn finish_provider_override(
        &mut self,
        context: Context,
        state_revision: u64,
        payload: Value,
        mut update: ProviderOverrideEntry,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let Some(current_document_revision) = payload["metadata"]["revision"].as_u64() else {
            return complete(send, context.request_id, malformed());
        };
        if state_revision != update.session.collection_revision {
            self.provider_sessions.remove(PLAYNITE_IMPORTER);
            return complete(
                send,
                context.request_id,
                revision_conflict(current_document_revision),
            );
        }
        if payload["found"] != true {
            if payload["found"] == false {
                return complete(
                    send,
                    context.request_id,
                    error(
                        "provider_entry_not_found",
                        "Playnite catalog entry was not found",
                    ),
                );
            }
            return complete(send, context.request_id, malformed());
        }
        if current_document_revision != update.session.document_revision
            || payload["metadata"]["next_id"].as_u64() != Some(update.session.next_id)
            || payload["metadata"]["provider_count"].as_u64() != Some(update.session.provider_count)
        {
            self.provider_sessions.remove(PLAYNITE_IMPORTER);
            return complete(
                send,
                context.request_id,
                revision_conflict(current_document_revision),
            );
        }
        let Some(entry_value) = payload.get("value").filter(|value| !value.is_null()) else {
            return complete(
                send,
                context.request_id,
                error(
                    "provider_entry_not_found",
                    "Playnite catalog entry was not found",
                ),
            );
        };
        let mut entry: Entry = match serde_json::from_value::<Entry>(entry_value.clone()) {
            Ok(entry) if entry.validate().is_ok() => entry,
            _ => return complete(send, context.request_id, malformed()),
        };
        if !matches!(entry.launch, Launch::Playnite { .. })
            || provider_entry_fingerprint(PLAYNITE_IMPORTER, &entry) != update.marker.fingerprint
            || entry.config_overrides != update.marker.config_overrides
        {
            return complete(
                send,
                context.request_id,
                error(
                    "provider_entry_changed",
                    "Playnite catalog entry differs from its provider marker",
                ),
            );
        }

        let requested_is_current = entry.config_overrides == update.config_overrides
            && update.marker.config_overrides == update.config_overrides;
        if update.expected_revision != current_document_revision {
            if requested_is_current {
                return complete(
                    send,
                    context.request_id,
                    json!({
                        "ok":true,
                        "id":update.id,
                        "revision":current_document_revision,
                        "config_overrides":update.config_overrides,
                        "changed":false,
                        "replayed":true,
                    }),
                );
            }
            return complete(
                send,
                context.request_id,
                revision_conflict(current_document_revision),
            );
        }
        if requested_is_current {
            return complete(
                send,
                context.request_id,
                json!({
                    "ok":true,
                    "id":update.id,
                    "revision":current_document_revision,
                    "config_overrides":update.config_overrides,
                    "changed":false,
                    "replayed":false,
                }),
            );
        }
        let Some(new_document_revision) = current_document_revision.checked_add(1) else {
            return complete(
                send,
                context.request_id,
                error("resource_exhausted", "catalog revision exhausted"),
            );
        };
        entry.config_overrides = update.config_overrides.clone();
        update.marker.config_overrides = update.config_overrides.clone();
        update
            .session
            .markers
            .insert(update.marker_key.clone(), update.marker.clone());
        update.session.document_revision = new_document_revision;
        let reply = json!({
            "ok":true,
            "id":update.id,
            "revision":new_document_revision,
            "config_overrides":update.config_overrides,
            "changed":true,
            "replayed":false,
        });
        let request = collection_commit(
            &update.session,
            vec![
                json!({"key":update.id,"value":entry}),
                json!({"key":update.marker_key,"value":marker_value(&update.marker)}),
            ],
        );
        self.submit(
            context,
            request,
            Stage::ProviderSaving(ProviderSave {
                session: update.session,
                reply,
                remaining_cleanup: Vec::new(),
                finish: false,
            }),
            send,
        )
    }

    fn reconcile_playnite_fullscreen_entry(
        &mut self,
        context: Context,
        session: ProviderSession,
        enabled: bool,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if session.caller != PLAYNITE_IMPORTER {
            return complete(
                send,
                context.request_id,
                error(
                    "permission_denied",
                    "only the fixed Playnite importer can reconcile its Fullscreen entry",
                ),
            );
        }
        let marker_key = entry_key(PLAYNITE_IMPORTER, PLAYNITE_FULLSCREEN_SOURCE_ID);
        let Some(marker) = session.markers.get(&marker_key).cloned() else {
            if !enabled {
                return complete(
                    send,
                    context.request_id,
                    json!({
                        "ok":true,
                        "enabled":false,
                        "changed":false,
                        "revision":session.document_revision,
                    }),
                );
            }
            return self.create_playnite_fullscreen_entry(context, session, send);
        };
        if !validate_marker(&marker, PLAYNITE_IMPORTER, PLAYNITE_FULLSCREEN_SOURCE_ID)
            || session.markers.values().any(|other| {
                other.entry_id == marker.entry_id && other.source_id != marker.source_id
            })
        {
            return complete(send, context.request_id, malformed());
        }
        let payload = json!({
            "action":"collection_get",
            "collection":session.key,
            "key":marker.entry_id,
        });
        self.submit(
            context,
            payload,
            Stage::ProviderFullscreenEntryRow(ProviderFullscreenEntry {
                session,
                enabled,
                marker_key,
                marker,
            }),
            send,
        )
    }

    fn create_playnite_fullscreen_entry(
        &mut self,
        context: Context,
        mut session: ProviderSession,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if session.entry_ids.len() >= MAX_ENTRIES || session.next_id == 0 {
            return complete(
                send,
                context.request_id,
                error("resource_exhausted", "catalog entry limit reached"),
            );
        }
        let entry_id = format!("app-{:016x}", session.next_id);
        if session.entry_ids.contains(&entry_id)
            || session
                .markers
                .values()
                .any(|marker| marker.entry_id == entry_id)
        {
            return complete(
                send,
                context.request_id,
                error("entry_conflict", "next catalog ID is already owned"),
            );
        }
        let Some(next_id) = session.next_id.checked_add(1) else {
            return complete(
                send,
                context.request_id,
                error("resource_exhausted", "catalog ID space exhausted"),
            );
        };
        let Some(document_revision) = session.document_revision.checked_add(1) else {
            return complete(
                send,
                context.request_id,
                error("resource_exhausted", "catalog revision exhausted"),
            );
        };
        if session.state.generation == 0 {
            // Markers require a generation. Seed provider state so the next
            // complete snapshot advances normally instead of treating this
            // fixed provider row as a future-generation marker.
            session.state.generation = 1;
            session.state.sync_id = "playnite-fullscreen-bootstrap".into();
        }
        if !session.state_present {
            session.provider_count = session.provider_count.saturating_add(1);
            session.state_present = true;
        }
        let entry = playnite_fullscreen_entry(ConfigOverrides::default());
        let marker = Marker {
            version: 1,
            caller: PLAYNITE_IMPORTER.into(),
            source_id: PLAYNITE_FULLSCREEN_SOURCE_ID.into(),
            entry_id: entry_id.clone(),
            generation: session.state.generation,
            fingerprint: provider_entry_fingerprint(PLAYNITE_IMPORTER, &entry),
            config_overrides: ConfigOverrides::default(),
            detached: false,
            remove_generation: None,
            added_at_unix: unix_timestamp(),
            source_flags: 0,
            remove_reason: None,
        };
        let marker_key = entry_key(PLAYNITE_IMPORTER, PLAYNITE_FULLSCREEN_SOURCE_ID);
        session.next_id = next_id;
        session.document_revision = document_revision;
        session.provider_count = session.provider_count.saturating_add(1);
        session.entry_ids.insert(entry_id.clone());
        session
            .fingerprints
            .insert(entry_id.clone(), marker.fingerprint.clone());
        session.markers.insert(marker_key.clone(), marker.clone());
        let reply = json!({
            "ok":true,
            "enabled":true,
            "changed":true,
            "revision":document_revision,
        });
        let request = collection_commit(
            &session,
            vec![
                json!({"key":entry_id,"value":entry}),
                json!({"key":marker_key,"value":marker_value(&marker)}),
                json!({"key":state_key(PLAYNITE_IMPORTER),"value":session.state.clone()}),
            ],
        );
        self.submit(
            context,
            request,
            Stage::ProviderSaving(ProviderSave {
                session,
                reply,
                remaining_cleanup: Vec::new(),
                finish: false,
            }),
            send,
        )
    }

    fn finish_playnite_fullscreen_entry(
        &mut self,
        context: Context,
        state_revision: u64,
        payload: Value,
        mut update: ProviderFullscreenEntry,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let Some(current_document_revision) = payload["metadata"]["revision"].as_u64() else {
            return complete(send, context.request_id, malformed());
        };
        if state_revision != update.session.collection_revision
            || current_document_revision != update.session.document_revision
            || payload["metadata"]["next_id"].as_u64() != Some(update.session.next_id)
            || payload["metadata"]["provider_count"].as_u64() != Some(update.session.provider_count)
        {
            self.provider_sessions.remove(PLAYNITE_IMPORTER);
            return complete(
                send,
                context.request_id,
                revision_conflict(current_document_revision),
            );
        }
        let Some(found) = payload["found"].as_bool() else {
            return complete(send, context.request_id, malformed());
        };
        let current_value = payload.get("value").filter(|value| !value.is_null());
        if !found && current_value.is_some() {
            return complete(send, context.request_id, malformed());
        }
        let current_entry = match current_value {
            Some(value) => match serde_json::from_value::<Entry>(value.clone()) {
                Ok(entry) if entry.validate().is_ok() => Some(entry),
                _ => return complete(send, context.request_id, malformed()),
            },
            None => None,
        };
        let expected_entry = playnite_fullscreen_entry(update.marker.config_overrides.clone());
        let entry_is_owned = current_entry.as_ref().is_some_and(|entry| {
            entry.name == PLAYNITE_FULLSCREEN_NAME
                && matches!(
                    entry.launch,
                    Launch::PlayniteFullscreen {
                        exit_timeout_seconds: 10,
                        ..
                    }
                )
                && entry.config_overrides == update.marker.config_overrides
                && provider_entry_fingerprint(PLAYNITE_IMPORTER, entry) == update.marker.fingerprint
                && provider_entry_fingerprint(PLAYNITE_IMPORTER, &expected_entry)
                    == update.marker.fingerprint
        });
        if update.enabled {
            if !found {
                // A deleted storage row with a surviving fixed marker can be
                // safely restored to its original ID and provider identity.
                return self.restore_playnite_fullscreen_entry(
                    context,
                    update,
                    current_document_revision,
                    send,
                );
            }
            if update.marker.detached || !entry_is_owned {
                return complete(
                    send,
                    context.request_id,
                    error(
                        "provider_entry_changed",
                        "reserved Playnite Fullscreen entry no longer matches its provider identity",
                    ),
                );
            }
            return complete(
                send,
                context.request_id,
                json!({
                    "ok":true,
                    "enabled":true,
                    "changed":false,
                    "revision":current_document_revision,
                }),
            );
        }

        let delete_entry = !update.marker.detached && entry_is_owned;
        let Some(provider_count) = update.session.provider_count.checked_sub(1) else {
            return complete(send, context.request_id, malformed());
        };
        if delete_entry {
            let Some(revision) = current_document_revision.checked_add(1) else {
                return complete(
                    send,
                    context.request_id,
                    error("resource_exhausted", "catalog revision exhausted"),
                );
            };
            update.session.document_revision = revision;
            update.session.entry_ids.remove(&update.marker.entry_id);
            update.session.fingerprints.remove(&update.marker.entry_id);
        }
        update.session.provider_count = provider_count;
        update.session.markers.remove(&update.marker_key);
        let mut edits = vec![json!({"key":update.marker_key,"value":null})];
        if delete_entry {
            edits.push(json!({"key":update.marker.entry_id,"value":null}));
        }
        let reply = json!({
            "ok":true,
            "enabled":false,
            "changed":true,
            "revision":update.session.document_revision,
        });
        let request = collection_commit(&update.session, edits);
        self.submit(
            context,
            request,
            Stage::ProviderSaving(ProviderSave {
                session: update.session,
                reply,
                remaining_cleanup: Vec::new(),
                finish: false,
            }),
            send,
        )
    }

    fn restore_playnite_fullscreen_entry(
        &mut self,
        context: Context,
        mut update: ProviderFullscreenEntry,
        current_document_revision: u64,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if update.marker.detached
            || !session_entry_id_available(&update.session, &update.marker.entry_id)
        {
            return complete(
                send,
                context.request_id,
                error(
                    "provider_entry_changed",
                    "reserved Playnite Fullscreen entry cannot be safely restored",
                ),
            );
        }
        let entry = playnite_fullscreen_entry(update.marker.config_overrides.clone());
        update.marker.fingerprint = provider_entry_fingerprint(PLAYNITE_IMPORTER, &entry);
        let Some(revision) = current_document_revision.checked_add(1) else {
            return complete(
                send,
                context.request_id,
                error("resource_exhausted", "catalog revision exhausted"),
            );
        };
        update.session.document_revision = revision;
        update.session.fingerprints.insert(
            update.marker.entry_id.clone(),
            update.marker.fingerprint.clone(),
        );
        let reply = json!({
            "ok":true,
            "enabled":true,
            "changed":true,
            "revision":revision,
        });
        let request = collection_commit(
            &update.session,
            vec![
                json!({"key":update.marker.entry_id,"value":entry}),
                json!({"key":update.marker_key,"value":marker_value(&update.marker)}),
            ],
        );
        self.submit(
            context,
            request,
            Stage::ProviderSaving(ProviderSave {
                session: update.session,
                reply,
                remaining_cleanup: Vec::new(),
                finish: false,
            }),
            send,
        )
    }

    fn begin_snapshot(
        &mut self,
        context: Context,
        mut session: ProviderSession,
        begin: ProviderBegin,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let ProviderBegin {
            staged,
            sync_id,
            source_revision,
            provider_epoch,
            expected_collection_revision,
        } = begin;
        if session.caller == PLAYNITE_IMPORTER && provider_epoch != session.state.provider_epoch {
            return complete(
                send,
                context.request_id,
                error("provider_epoch_conflict", "Playnite provider epoch changed"),
            );
        }
        if session.caller != PLAYNITE_IMPORTER && provider_epoch != 0 {
            return complete(
                send,
                context.request_id,
                error("invalid_request", "provider epoch is reserved for Playnite"),
            );
        }
        if expected_collection_revision
            .is_some_and(|revision| revision != session.collection_revision)
        {
            return complete(
                send,
                context.request_id,
                json!({"ok":false,"error":{"code":"revision_conflict","message":"catalog changed after provider inventory","current_collection_revision":session.collection_revision}}),
            );
        }
        if !valid_sync_id(&sync_id) {
            return complete(
                send,
                context.request_id,
                error(
                    "invalid_request",
                    "sync_id must contain 1..128 printable bytes",
                ),
            );
        }
        if session.state.sync_id == sync_id {
            if session.state.revoked {
                return complete(
                    send,
                    context.request_id,
                    error("snapshot_conflict", "revoked snapshot cannot be resumed"),
                );
            }
            if session.state.source_revision != source_revision || session.state.staged != staged {
                return complete(
                    send,
                    context.request_id,
                    error(
                        "snapshot_conflict",
                        "sync_id was reused for another source revision",
                    ),
                );
            }
            self.provider_sessions
                .insert(session.caller.clone(), session.clone());
            return complete(
                send,
                context.request_id,
                json!({"ok":true,"revision":session.document_revision,"generation":session.state.generation,"provider_epoch":provider_epoch,"next_sequence":session.state.next_sequence,"staged":session.state.staged}),
            );
        }
        let is_new_state = !session.state_present;
        let Some(generation) = session.state.generation.checked_add(1) else {
            return complete(
                send,
                context.request_id,
                error("resource_exhausted", "provider generation space exhausted"),
            );
        };
        session.state = SnapshotState {
            version: 1,
            staged,
            staged_at_unix: staged.then(unix_timestamp).flatten(),
            staged_intents: StagedIntents::default(),
            provider_epoch,
            last_revocation_id: session.state.last_revocation_id.clone(),
            revoked: false,
            generation,
            sync_id,
            source_revision,
            next_sequence: 0,
            last_batch: None,
            finished: false,
            cleanup_pending: false,
            finish_expected_revision: None,
            finish_remove_stale: true,
            finish_remove_uninstalled: false,
            finish_remove_expired: false,
            finish_remove_selection: false,
        };
        session.seen_source_ids.clear();
        let key = state_key(&session.caller);
        if is_new_state {
            session.provider_count = session.provider_count.saturating_add(1);
            session.state_present = true;
        }
        let reply = json!({"ok":true,"revision":session.document_revision,"generation":generation,"provider_epoch":provider_epoch,"next_sequence":0,"staged":staged});
        let mut edits = vec![json!({"key":key,"value":session.state.clone()})];
        edits.extend(reclaim_abandoned_batches(&mut session));
        let payload = collection_commit(&session, edits);
        self.submit(
            context,
            payload,
            Stage::ProviderSaving(ProviderSave {
                session,
                reply,
                remaining_cleanup: Vec::new(),
                finish: false,
            }),
            send,
        )
    }

    fn revoke_snapshot(
        &mut self,
        context: Context,
        mut session: ProviderSession,
        revocation_id: String,
        expected_provider_epoch: u64,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if session.caller != PLAYNITE_IMPORTER {
            return complete(
                send,
                context.request_id,
                error(
                    "permission_denied",
                    "only Playnite may revoke its provider epoch",
                ),
            );
        }
        if !valid_sync_id(&revocation_id) {
            return complete(
                send,
                context.request_id,
                error("invalid_request", "invalid revocation ID"),
            );
        }
        if session.state.last_revocation_id.as_deref() == Some(revocation_id.as_str()) {
            return if expected_provider_epoch.checked_add(1) == Some(session.state.provider_epoch) {
                complete(
                    send,
                    context.request_id,
                    json!({"ok":true,"provider_epoch":session.state.provider_epoch,"revision":session.document_revision,"replayed":true}),
                )
            } else {
                complete(
                    send,
                    context.request_id,
                    error(
                        "provider_epoch_conflict",
                        "revocation ID was reused for another epoch",
                    ),
                )
            };
        }
        if expected_provider_epoch != session.state.provider_epoch {
            return complete(
                send,
                context.request_id,
                error("provider_epoch_conflict", "Playnite provider epoch changed"),
            );
        }
        let Some(next_epoch) = session.state.provider_epoch.checked_add(1) else {
            return complete(
                send,
                context.request_id,
                error("resource_exhausted", "provider epoch exhausted"),
            );
        };
        session.state.provider_epoch = next_epoch;
        session.state.last_revocation_id = Some(revocation_id);
        session.state.revoked = true;
        if !session.state_present {
            session.provider_count = session.provider_count.saturating_add(1);
            session.state_present = true;
        }
        let payload = collection_commit(
            &session,
            vec![json!({"key":state_key(&session.caller),"value":session.state.clone()})],
        );
        let reply = json!({"ok":true,"provider_epoch":next_epoch,"revision":session.document_revision,"replayed":false});
        self.submit(
            context,
            payload,
            Stage::ProviderSaving(ProviderSave {
                session,
                reply,
                remaining_cleanup: Vec::new(),
                finish: false,
            }),
            send,
        )
    }

    fn commit_provider_batch(
        &mut self,
        context: Context,
        session: ProviderSession,
        batch: ProviderBatch,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let (session, edits) = match plan_provider_batch(session, batch) {
            Ok(plan) => plan,
            Err(failure) => return complete(send, context.request_id, failure),
        };
        let payload = collection_commit(&session, edits);
        let reply = json!({"ok":true,"revision":session.document_revision,"generation":session.state.generation,"provider_epoch":session.state.provider_epoch,"next_sequence":session.state.next_sequence});
        self.submit(
            context,
            payload,
            Stage::ProviderSaving(ProviderSave {
                session,
                reply,
                remaining_cleanup: Vec::new(),
                finish: false,
            }),
            send,
        )
    }

    fn stage_provider_batch(
        &mut self,
        context: Context,
        mut session: ProviderSession,
        batch: ProviderBatch,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let sequence = batch.sequence;
        let expected_revision = batch.expected_revision;
        let fingerprint = batch.fingerprint.clone();
        let row = StagedIntents {
            entries: batch.entries,
            remove_source_ids: batch.remove_source_ids,
            expire_source_ids: batch.expire_source_ids,
            selection_remove_source_ids: batch.selection_remove_source_ids,
        };
        let source_ids = match validate_staged_intents(
            &session.caller,
            session.provider_key.as_deref(),
            &session.seen_source_ids,
            &row,
        ) {
            Ok(ids) => ids,
            Err(failure) => return complete(send, context.request_id, failure),
        };
        session.seen_source_ids.extend(source_ids);
        let Some(next_sequence) = sequence.checked_add(1) else {
            return complete(
                send,
                context.request_id,
                error("resource_exhausted", "provider sequence space exhausted"),
            );
        };
        session.state.next_sequence = next_sequence;
        session.state.last_batch = Some(CommittedBatch {
            sequence,
            expected_revision,
            revision: session.document_revision,
            next_sequence,
            fingerprint,
        });
        let key = staged_key(&session.caller, session.state.generation, sequence);
        let cleanup = reclaim_abandoned_batches(&mut session);
        if session.staged_rows.len() >= 12_500 {
            return complete(
                send,
                context.request_id,
                error("resource_exhausted", "too many staged batches"),
            );
        }
        session.provider_count = match session.provider_count.checked_add(1) {
            Some(count) => count,
            None => {
                return complete(
                    send,
                    context.request_id,
                    error("resource_exhausted", "provider count exhausted"),
                );
            }
        };
        session.staged_rows.insert(key.clone(), row.clone());
        let mut edits = vec![
            json!({"key":state_key(&session.caller), "value":session.state}),
            json!({"key":key, "value":row}),
        ];
        edits.extend(cleanup);
        let payload = collection_commit(&session, edits);
        let reply = json!({"ok":true,"revision":session.document_revision,"generation":session.state.generation,"provider_epoch":session.state.provider_epoch,"next_sequence":next_sequence});
        self.submit(
            context,
            payload,
            Stage::ProviderSaving(ProviderSave {
                session,
                reply,
                remaining_cleanup: Vec::new(),
                finish: false,
            }),
            send,
        )
    }

    fn publish_staged_snapshot(
        &mut self,
        context: Context,
        mut session: ProviderSession,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let remove_stale = session.state.finish_remove_stale;
        let remove_uninstalled = session.state.finish_remove_uninstalled;
        let remove_expired = session.state.finish_remove_expired;
        let remove_selection = session.state.finish_remove_selection;
        let original_revision = session.document_revision;
        let finish_state = session.state.clone();
        let intents = match collect_staged(&session) {
            Ok(intents) => intents,
            Err(failure) => return complete(send, context.request_id, failure),
        };
        session.seen_source_ids.clear();
        let planned = staged_batch(&intents, original_revision);
        let (mut session, edits) = match plan_provider_batch(session, planned) {
            Ok(result) => result,
            Err(failure) => return complete(send, context.request_id, failure),
        };
        session.state = finish_state;
        let batch_changed = session.document_revision != original_revision;
        session.document_revision = original_revision;
        let stale = plan_stale_markers(
            &session,
            session.state.generation,
            remove_stale,
            remove_uninstalled,
            remove_expired,
            remove_selection,
        );
        let cleanup = match plan_cleanup_edits(&mut session, stale) {
            Ok(edits) => edits,
            Err(failure) => return complete(send, context.request_id, failure),
        };
        let changed = batch_changed || session.document_revision != original_revision;
        session.document_revision = match original_revision.checked_add(u64::from(changed)) {
            Some(revision) => revision,
            None => {
                return complete(
                    send,
                    context.request_id,
                    error("resource_exhausted", "catalog revision exhausted"),
                );
            }
        };
        session.state.staged_intents = StagedIntents::default();
        session.state.staged_at_unix = None;
        session.state.cleanup_pending = false;
        let mut unique = BTreeMap::new();
        for edit in edits.into_iter().chain(cleanup) {
            let key = edit["key"].as_str().expect("planned edit key").to_owned();
            unique.insert(key, edit);
        }
        unique.insert(
            state_key(&session.caller),
            json!({"key":state_key(&session.caller), "value":session.state}),
        );
        for key in session.staged_rows.keys() {
            unique.insert(key.clone(), json!({"key":key,"value":null}));
        }
        session.provider_count = match session
            .provider_count
            .checked_sub(session.staged_rows.len() as u64)
        {
            Some(count) => count,
            None => return complete(send, context.request_id, malformed()),
        };
        session.staged_rows.clear();
        let mut chunks = Vec::new();
        let mut chunk = Vec::new();
        for edit in unique.into_values() {
            chunk.push(edit);
            if serde_json::to_vec(&chunk).map_or(true, |bytes| bytes.len() > 11_500) {
                let last = chunk.pop().expect("one edit");
                if chunk.is_empty() {
                    return complete(
                        send,
                        context.request_id,
                        error("resource_exhausted", "publication edit exceeds draft limit"),
                    );
                }
                chunks.push(std::mem::take(&mut chunk));
                chunk.push(last);
            }
            if chunk.len() == 128 {
                chunks.push(std::mem::take(&mut chunk));
            }
        }
        if !chunk.is_empty() {
            chunks.push(chunk);
        }
        if chunks.len() > 1024
            || chunks
                .iter()
                .map(|chunk| serde_json::to_vec(chunk).map_or(usize::MAX, |b| b.len()))
                .sum::<usize>()
                > 16 * 1024 * 1024
        {
            return complete(
                send,
                context.request_id,
                error(
                    "resource_exhausted",
                    "staged publication exceeds draft limits",
                ),
            );
        }
        let reply = json!({"ok":true,"revision":session.document_revision,"generation":session.state.generation,"provider_epoch":session.state.provider_epoch,"complete":true,"cleanup_pending":false,"remove_stale":remove_stale,"remove_uninstalled":remove_uninstalled,"remove_expired":remove_expired,"remove_selection":remove_selection});
        let draft_id = fingerprint_bytes(format!("{}:{}", session.key, session.caller).as_bytes());
        // Bind the resumable draft to the exact chunk bytes and resulting metadata.
        // The State store compares this digest before accepting a resumed prefix.
        let plan = json!({"chunks":chunks,"metadata":{"revision":session.document_revision,"next_id":session.next_id,"provider_count":session.provider_count}});
        let content_id = match serde_json::to_vec(&plan) {
            Ok(bytes) => fingerprint_bytes(&bytes),
            Err(_) => return complete(send, context.request_id, malformed()),
        };
        let payload = json!({"action":"collection_draft_begin", "collection":session.key, "draft_id":draft_id, "content_id":content_id, "expected_revision":session.collection_revision});
        self.submit(
            context,
            payload,
            Stage::DraftBegin(DraftPublish {
                save: ProviderSave {
                    session,
                    reply,
                    remaining_cleanup: Vec::new(),
                    finish: true,
                },
                draft_id,
                content_id,
                chunks,
                next: 0,
            }),
            send,
        )
    }

    fn continue_draft(
        &mut self,
        context: Context,
        mut draft: DraftPublish,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if draft.next < draft.chunks.len() {
            let payload = json!({"action":"collection_draft_append", "collection":draft.save.session.key, "draft_id":draft.draft_id, "sequence":draft.next, "edits":draft.chunks[draft.next]});
            draft.next += 1;
            self.submit(context, payload, Stage::DraftAppend(draft), send)
        } else {
            let session = &draft.save.session;
            let payload = json!({"action":"collection_draft_publish", "collection":session.key, "draft_id":draft.draft_id, "expected_revision":session.collection_revision, "metadata":{"revision":session.document_revision,"next_id":session.next_id,"provider_count":session.provider_count}});
            self.submit(context, payload, Stage::DraftPublishing(draft), send)
        }
    }

    fn finish_snapshot(
        &mut self,
        context: Context,
        mut session: ProviderSession,
        finish: ProviderFinish,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let ProviderFinish {
            sync_id,
            generation,
            source_revision,
            provider_epoch,
            expected_revision,
            next_sequence,
            source_complete,
            remove_stale,
            remove_uninstalled,
            remove_expired,
            remove_selection,
        } = finish;
        if !source_complete {
            return complete(
                send,
                context.request_id,
                error(
                    "incomplete_snapshot",
                    "incomplete provider scans cannot be finished",
                ),
            );
        }
        if !snapshot_matches(
            &session,
            &sync_id,
            generation,
            source_revision,
            provider_epoch,
        ) || next_sequence != session.state.next_sequence
        {
            return complete(
                send,
                context.request_id,
                error(
                    "snapshot_conflict",
                    "provider snapshot generation or sequence differs",
                ),
            );
        }
        if session.state.finished {
            if session.state.finish_expected_revision != Some(expected_revision)
                || session.state.finish_remove_stale != remove_stale
                || session.state.finish_remove_uninstalled != remove_uninstalled
                || session.state.finish_remove_expired != remove_expired
                || session.state.finish_remove_selection != remove_selection
            {
                return complete(
                    send,
                    context.request_id,
                    error("snapshot_conflict", "finish retry revision differs"),
                );
            }
            if !session.state.cleanup_pending {
                return complete(
                    send,
                    context.request_id,
                    json!({"ok":true,"revision":session.document_revision,"generation":generation,"provider_epoch":provider_epoch,"complete":true,"cleanup_pending":false,"remove_stale":remove_stale,"remove_uninstalled":remove_uninstalled,"remove_expired":remove_expired,"remove_selection":remove_selection}),
                );
            }
        } else {
            if expected_revision != session.document_revision {
                return complete(
                    send,
                    context.request_id,
                    revision_conflict(session.document_revision),
                );
            }
            session.state.finished = true;
            session.state.finish_expected_revision = Some(expected_revision);
            session.state.finish_remove_stale = remove_stale;
            session.state.finish_remove_uninstalled = remove_uninstalled;
            session.state.finish_remove_expired = remove_expired;
            session.state.finish_remove_selection = remove_selection;
        }
        if session.state.staged {
            return self.publish_staged_snapshot(context, session, send);
        }
        let stale = plan_stale_markers(
            &session,
            generation,
            remove_stale,
            remove_uninstalled,
            remove_expired,
            remove_selection,
        );
        session.state.cleanup_pending = !stale.is_empty();
        self.commit_provider_cleanup(context, session, stale, send)
    }

    fn commit_provider_cleanup(
        &mut self,
        context: Context,
        mut session: ProviderSession,
        mut stale: Vec<Marker>,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        const CLEANUP_BATCH: usize = 63;
        let count = stale.len().min(CLEANUP_BATCH);
        let remaining = stale.split_off(count);
        let mut edits = match plan_cleanup_edits(&mut session, stale) {
            Ok(edits) => edits,
            Err(failure) => return complete(send, context.request_id, failure),
        };
        session.state.cleanup_pending = !remaining.is_empty();
        edits.push(json!({
            "key":state_key(&session.caller),
            "value":session.state.clone(),
        }));
        let payload = collection_commit(&session, edits);
        let reply = json!({"ok":true,"revision":session.document_revision,"generation":session.state.generation,"provider_epoch":session.state.provider_epoch,"complete":true,"cleanup_pending":!remaining.is_empty(),"remove_stale":session.state.finish_remove_stale,"remove_uninstalled":session.state.finish_remove_uninstalled,"remove_expired":session.state.finish_remove_expired,"remove_selection":session.state.finish_remove_selection});
        self.submit(
            context,
            payload,
            Stage::ProviderSaving(ProviderSave {
                session,
                reply,
                remaining_cleanup: remaining,
                finish: true,
            }),
            send,
        )
    }
    fn submit(
        &mut self,
        context: Context,
        payload: Value,
        stage: Stage,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if matches!(&stage, Stage::ProviderSaving(_)) {
            // The outcome may be unknown even when the transport fails. Do
            // not retain any view of a collection whose commit was attempted.
            self.provider_sessions
                .retain(|_, session| session.key != context.key);
        }
        let remaining_ms = context
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if remaining_ms == 0 {
            return complete(
                send,
                context.request_id,
                error("deadline_exceeded", "catalog request deadline elapsed"),
            );
        }
        self.sequence = self.sequence.wrapping_add(1);
        match send(Command::Request {
            service: STATE_SERVICE.into(),
            payload,
            timeout_ms: remaining_ms.min(30_000),
            operation_id: format!("catalog.{}.{}", context.request_id, self.sequence),
            parent: Some(context.request_id),
        })? {
            Response::Accepted { id } => {
                self.pending = Some(Pending {
                    context,
                    state_request_id: id,
                    stage,
                });
                Ok(())
            }
            failure => complete(send, context.request_id, runtime_failure(failure)),
        }
    }
    fn advance(
        &mut self,
        pending: Pending,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        match send(Command::TakeResult {
            request: pending.state_request_id,
        })? {
            Response::Pending => {
                self.pending = Some(pending);
                Ok(())
            }
            Response::Result { payload } => {
                if matches!(
                    &pending.stage,
                    Stage::ProviderSaving(_) | Stage::DraftPublishing(_)
                ) {
                    self.provider_sessions
                        .retain(|_, session| session.key != pending.context.key);
                }
                if payload["ok"] != true {
                    if payload["error"]["code"] == "draft_conflict"
                        && let Stage::DraftBegin(draft) = pending.stage
                    {
                        let abort = json!({"action":"collection_draft_abort","collection":draft.save.session.key,"draft_id":draft.draft_id});
                        return self.submit(pending.context, abort, Stage::DraftReset(draft), send);
                    }
                    if payload["error"]["code"] == "sequence_conflict"
                        && let Stage::DraftVerify(draft) = pending.stage
                    {
                        let abort = json!({"action":"collection_draft_abort","collection":draft.save.session.key,"draft_id":draft.draft_id});
                        return self.submit(pending.context, abort, Stage::DraftReset(draft), send);
                    }
                    let failure = if payload["ok"] == false && payload["error"].is_object() {
                        payload
                    } else {
                        malformed()
                    };
                    return complete(send, pending.context.request_id, failure);
                }
                let revision = if matches!(
                    pending.stage,
                    Stage::DraftReset(_)
                        | Stage::DraftBegin(_)
                        | Stage::DraftAppend(_)
                        | Stage::DraftVerify(_)
                ) {
                    0
                } else if let Some(revision) = payload["revision"].as_u64() {
                    revision
                } else {
                    return complete(send, pending.context.request_id, malformed());
                };
                self.consume(pending.context, pending.stage, revision, payload, send)
            }
            failure => {
                if matches!(
                    &pending.stage,
                    Stage::ProviderSaving(_) | Stage::DraftPublishing(_)
                ) {
                    self.provider_sessions
                        .retain(|_, session| session.key != pending.context.key);
                }
                complete(send, pending.context.request_id, runtime_failure(failure))
            }
        }
    }
    fn consume(
        &mut self,
        mut context: Context,
        stage: Stage,
        state_revision: u64,
        payload: Value,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        match stage {
            Stage::DraftReset(draft) => {
                let payload = json!({"action":"collection_draft_begin", "collection":draft.save.session.key, "draft_id":draft.draft_id, "content_id":draft.content_id, "expected_revision":draft.save.session.collection_revision});
                self.submit(context, payload, Stage::DraftBegin(draft), send)
            }
            Stage::DraftPublishing(draft) => self.consume(
                context,
                Stage::ProviderSaving(draft.save),
                state_revision,
                payload,
                send,
            ),
            Stage::DraftBegin(mut draft) => {
                let Some(next) = payload["next_sequence"]
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                else {
                    return complete(send, context.request_id, malformed());
                };
                if next > draft.chunks.len() {
                    return complete(send, context.request_id, malformed());
                }
                draft.next = next;
                if next > 0 {
                    let payload = json!({"action":"collection_draft_append", "collection":draft.save.session.key, "draft_id":draft.draft_id, "sequence":next - 1, "edits":draft.chunks[next - 1]});
                    return self.submit(context, payload, Stage::DraftVerify(draft), send);
                }
                self.continue_draft(context, draft, send)
            }
            Stage::DraftVerify(draft) => {
                if payload["next_sequence"].as_u64() != Some(draft.next as u64) {
                    return complete(send, context.request_id, malformed());
                }
                self.continue_draft(context, draft, send)
            }
            Stage::DraftAppend(draft) => {
                if payload["next_sequence"].as_u64() != Some(draft.next as u64) {
                    return complete(send, context.request_id, malformed());
                }
                self.continue_draft(context, draft, send)
            }
            Stage::Saving(reply) => {
                self.provider_sessions
                    .retain(|_, session| session.key != context.key);
                complete(send, context.request_id, reply)
            }
            Stage::LaunchMarkerPage(scan) => {
                self.consume_launch_marker_page(context, scan, state_revision, payload, send)
            }
            Stage::ProviderSaving(mut save) => {
                save.session.collection_revision = state_revision;
                // A provider commit changes the collection shared by every
                // importer for this owner. Discard all cached views, including
                // this one before a cleanup continuation can fail to submit.
                self.provider_sessions
                    .retain(|_, session| session.key != context.key);
                if save.finish && !save.remaining_cleanup.is_empty() {
                    return self.commit_provider_cleanup(
                        context,
                        save.session,
                        save.remaining_cleanup,
                        send,
                    );
                }
                if let Some(caller) = &context.importer {
                    self.provider_sessions.insert(caller.clone(), save.session);
                }
                complete(send, context.request_id, save.reply)
            }
            Stage::ProviderOverrideEntry(update) => {
                self.finish_provider_override(context, state_revision, payload, *update, send)
            }
            Stage::ArtworkMarker { source_id, artwork } => {
                self.read_artwork_entry(context, state_revision, payload, source_id, artwork, send)
            }
            Stage::ArtworkEntry(update) => {
                self.finish_artwork_refresh(context, state_revision, payload, *update, send)
            }
            Stage::ProviderFullscreenEntryRow(update) => self.finish_playnite_fullscreen_entry(
                context,
                state_revision,
                payload,
                update,
                send,
            ),
            Stage::ProviderMarkerCheck {
                operation,
                state_revision,
                document,
                id,
            } => {
                if payload["found"] != true && payload["found"] != false {
                    return complete(send, context.request_id, malformed());
                }
                if let Some(value) = payload.get("value").filter(|value| !value.is_null()) {
                    let marker: Marker = match serde_json::from_value(value.clone()) {
                        Ok(marker)
                            if validate_marker(
                                &marker,
                                PLAYNITE_IMPORTER,
                                PLAYNITE_FULLSCREEN_SOURCE_ID,
                            ) =>
                        {
                            marker
                        }
                        _ => return complete(send, context.request_id, malformed()),
                    };
                    if marker.entry_id == id && !marker.detached {
                        return complete(
                            send,
                            context.request_id,
                            error(
                                "provider_entry_managed",
                                "reserved Playnite Fullscreen entries can only be changed through provider settings",
                            ),
                        );
                    }
                }
                self.operate(context, operation, state_revision, document, send)
            }
            Stage::StagedCacheCheck { operation, session } => {
                if state_revision == session.collection_revision {
                    self.apply_provider_operation(context, operation, session, send)
                } else {
                    self.consume(
                        context,
                        Stage::ProviderLoading(operation),
                        state_revision,
                        payload,
                        send,
                    )
                }
            }
            Stage::ProviderInventoryCheck { operation, session } => {
                if state_revision != session.collection_revision {
                    return complete(
                        send,
                        context.request_id,
                        json!({"ok":false,"error":{"code":"revision_conflict","message":"catalog changed between inventory pages","current_collection_revision":state_revision}}),
                    );
                }
                self.apply_provider_operation(context, operation, session, send)
            }
            Stage::ProviderLoading(operation) => {
                let found = payload["found"] == true;
                if payload["found"] != true && payload["found"] != false {
                    return complete(send, context.request_id, malformed());
                }
                let metadata = if found {
                    payload["metadata"].clone()
                } else {
                    json!({"revision":0,"next_id":1,"provider_count":0})
                };
                let Some(total_count) = payload["count"].as_u64().or_else(|| (!found).then_some(0))
                else {
                    return complete(send, context.request_id, malformed());
                };
                let Some(provider_count) = metadata["provider_count"]
                    .as_u64()
                    .or_else(|| metadata.get("provider_count").is_none().then_some(0))
                else {
                    return complete(send, context.request_id, malformed());
                };
                let document = match header(&metadata) {
                    Ok(document) => document,
                    Err(failure) => return complete(send, context.request_id, failure),
                };
                let scan = ProviderScan {
                    operation,
                    state_revision,
                    document_revision: document.revision,
                    next_id: document.next_id,
                    total_count,
                    provider_count,
                    hidden_rows: 0,
                    after: None,
                    scanned: 0,
                    fingerprints: BTreeMap::new(),
                    entry_names: BTreeMap::new(),
                    artwork_entries: BTreeMap::new(),
                    entry_ids: BTreeSet::new(),
                    markers: BTreeMap::new(),
                    marker_entry_ids: BTreeSet::new(),
                    provider_state: None,
                    staged_rows: BTreeMap::new(),
                };
                self.scan_provider_page(context, scan, send)
            }
            Stage::ProviderPaging(mut scan) => {
                if payload["revision"] != scan.state_revision {
                    return complete(send, context.request_id, malformed());
                }
                let Some(entries) = payload["entries"].as_array() else {
                    return complete(send, context.request_id, malformed());
                };
                for row in entries {
                    let Some(key) = row["key"].as_str() else {
                        return complete(send, context.request_id, malformed());
                    };
                    if scan.after.as_deref().is_some_and(|after| key <= after) {
                        return complete(send, context.request_id, malformed());
                    }
                    scan.after = Some(key.into());
                    scan.scanned = scan.scanned.saturating_add(1);
                    if scan.scanned > 100_000 {
                        return complete(
                            send,
                            context.request_id,
                            error("resource_exhausted", "catalog snapshot exceeds item limit"),
                        );
                    }
                    if let Some((caller, source_id)) = parse_entry_key(key) {
                        let marker: Marker = match serde_json::from_value(row["value"].clone()) {
                            Ok(marker) if validate_marker(&marker, caller, source_id) => marker,
                            _ => return complete(send, context.request_id, malformed()),
                        };
                        if !scan.marker_entry_ids.insert(marker.entry_id.clone()) {
                            return complete(send, context.request_id, malformed());
                        }
                        scan.markers.insert(key.into(), marker);
                        scan.hidden_rows += 1;
                    } else if key.starts_with(STAGED_PREFIX) {
                        scan.hidden_rows += 1;
                        let Some((caller, _, _)) = parse_staged_key(key) else {
                            return complete(send, context.request_id, malformed());
                        };
                        if Some(caller) == context.importer.as_deref() {
                            let row: StagedIntents =
                                match serde_json::from_value(row["value"].clone()) {
                                    Ok(row) => row,
                                    Err(_) => {
                                        return complete(send, context.request_id, malformed());
                                    }
                                };
                            scan.staged_rows.insert(key.into(), row);
                        }
                    } else if key == seed::MARKER_KEY {
                        scan.hidden_rows += 1;
                    } else if let Some(caller) = key.strip_prefix(STATE_PREFIX) {
                        scan.hidden_rows += 1;
                        if Some(caller) == context.importer.as_deref() {
                            let state: SnapshotState =
                                match serde_json::from_value::<SnapshotState>(row["value"].clone())
                                {
                                    Ok(state) if state.validate() => state,
                                    _ => return complete(send, context.request_id, malformed()),
                                };
                            scan.provider_state = Some(state);
                        }
                    } else if crate::model::parse_id(key).is_some() {
                        if crate::model::parse_id(key).is_some_and(|id| id >= scan.next_id) {
                            return complete(send, context.request_id, malformed());
                        }
                        let entry: Entry =
                            match serde_json::from_value::<Entry>(row["value"].clone()) {
                                Ok(entry) if entry.validate().is_ok() => entry,
                                _ => return complete(send, context.request_id, malformed()),
                            };
                        if scan
                            .fingerprints
                            .insert(
                                key.into(),
                                provider_entry_fingerprint(
                                    context.importer.as_deref().unwrap_or_default(),
                                    &entry,
                                ),
                            )
                            .is_some()
                        {
                            return complete(send, context.request_id, malformed());
                        }
                        scan.entry_names.insert(key.into(), entry.name.clone());
                        if matches!(entry.launch, Launch::Playnite { .. }) {
                            scan.artwork_entries.insert(key.into(), entry);
                        }
                        scan.entry_ids.insert(key.into());
                    } else {
                        return complete(send, context.request_id, malformed());
                    }
                }
                if let Some(next) = payload["next"].as_str() {
                    if entries.is_empty() || scan.after.as_deref() != Some(next) {
                        return complete(send, context.request_id, malformed());
                    }
                    self.scan_provider_page(context, scan, send)
                } else {
                    if scan.total_count != scan.hidden_rows + scan.entry_ids.len() as u64
                        || scan.provider_count != scan.hidden_rows
                    {
                        return complete(send, context.request_id, malformed());
                    }
                    let caller = context.importer.as_deref().expect("provider caller");
                    if scan.markers.values().any(|marker| {
                        crate::model::parse_id(&marker.entry_id).is_none_or(|id| id >= scan.next_id)
                    }) {
                        return complete(send, context.request_id, malformed());
                    }
                    if scan.markers.values().any(|marker| {
                        marker.caller == caller
                            && scan
                                .provider_state
                                .as_ref()
                                .is_none_or(|state| marker.generation > state.generation)
                    }) {
                        return complete(send, context.request_id, malformed());
                    }
                    self.finish_provider_scan(context, scan, send)
                }
            }
            Stage::Migrating(operation) => self.load(context, operation, send),
            Stage::Legacy(operation) => self.migrate(context, operation, payload, send),
            Stage::Paging(scan) => self.consume_page(context, scan, payload, send),
            Stage::Loading(operation) => {
                if payload["found"] == false {
                    let request = json!({"action":"get","key":context.key});
                    return self.submit(context, request, Stage::Legacy(operation), send);
                }
                if payload["found"] != true {
                    return complete(send, context.request_id, malformed());
                }
                let Some(count) = payload["count"].as_u64() else {
                    return complete(send, context.request_id, malformed());
                };
                let Some(provider_count) =
                    payload["metadata"]["provider_count"].as_u64().or_else(|| {
                        payload["metadata"]
                            .get("provider_count")
                            .is_none()
                            .then_some(0)
                    })
                else {
                    return complete(send, context.request_id, malformed());
                };
                if provider_count > count {
                    return complete(send, context.request_id, malformed());
                }
                let mut document = match header(&payload["metadata"]) {
                    Ok(value) => value,
                    Err(failure) => return complete(send, context.request_id, failure),
                };
                if let Operation::Get { id }
                | Operation::ResolveLaunch { id }
                | Operation::ResolveExported { id }
                | Operation::ResolveExportedArtwork { id }
                | Operation::ResolveExportedLaunch { id }
                | Operation::Update { id, .. }
                | Operation::Delete { id, .. }
                | Operation::ProfileUpsert { id, .. }
                | Operation::ProfileDelete { id, .. } = &operation
                    && !payload["value"].is_null()
                {
                    let entry: Entry = match serde_json::from_value(payload["value"].clone()) {
                        Ok(value) => value,
                        Err(_) => return complete(send, context.request_id, malformed()),
                    };
                    document.entries.insert(id.clone(), entry);
                    if document.validate().is_err() {
                        return complete(send, context.request_id, malformed());
                    }
                }
                context.provider_count = provider_count;
                if let Operation::Update { id, .. } | Operation::Delete { id, .. } = &operation {
                    let id = id.clone();
                    let payload = json!({
                        "action":"collection_get",
                        "collection":context.key,
                        "key":entry_key(PLAYNITE_IMPORTER, PLAYNITE_FULLSCREEN_SOURCE_ID),
                    });
                    return self.submit(
                        context,
                        payload,
                        Stage::ProviderMarkerCheck {
                            operation,
                            state_revision,
                            document,
                            id,
                        },
                        send,
                    );
                }
                let public_count = count.saturating_sub(context.provider_count);
                if (matches!(operation, Operation::Create { .. })
                    || matches!(&operation,Operation::ProfileUpsert {id,..} if id.is_empty()))
                    && public_count >= MAX_ENTRIES as u64
                {
                    return complete(
                        send,
                        context.request_id,
                        error("resource_exhausted", "catalog entry limit reached"),
                    );
                }
                self.operate(context, operation, state_revision, document, send)
            }
        }
    }
    fn migrate(
        &mut self,
        context: Context,
        operation: Operation,
        payload: Value,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if payload["found"] == false {
            if matches!(
                operation,
                Operation::ExportList { .. }
                    | Operation::ExportIdentityInventory { .. }
                    | Operation::ProfileList { .. }
                    | Operation::ExportSteamIds { .. }
                    | Operation::ResolveExported { .. }
                    | Operation::ResolveExportedArtwork { .. }
                    | Operation::ResolveExportedLaunch { .. }
            ) {
                return complete_read(send, context, operation, Document::default());
            }
            return self.operate(context, operation, 0, Document::default(), send);
        }
        let document: Document = match serde_json::from_value(payload["value"].clone()) {
            Ok(value) => value,
            Err(_) => return complete(send, context.request_id, malformed()),
        };
        if document.validate().is_err() {
            return complete(send, context.request_id, malformed());
        }
        // Export must remain read-only even when ordinary reads would migrate legacy data.
        if matches!(
            operation,
            Operation::ExportList { .. }
                | Operation::ExportIdentityInventory { .. }
                | Operation::ProfileList { .. }
                | Operation::ExportSteamIds { .. }
                | Operation::ResolveExported { .. }
                | Operation::ResolveExportedArtwork { .. }
                | Operation::ResolveExportedLaunch { .. }
        ) {
            return complete_read(send, context, operation, document);
        }
        let metadata = json!({"revision":document.revision,"next_id":document.next_id,"provider_count":context.provider_count});
        let edits: Vec<_> = document
            .entries
            .into_iter()
            .map(|(key, value)| json!({"key":key,"value":value}))
            .collect();
        // Legacy documents fit one transport packet; migration publishes all
        // entries and their original metadata with a single manifest CAS.
        let payload = json!({"action":"collection_commit","collection":context.key,"expected_revision":0,"metadata":metadata,"edits":edits});
        self.submit(context, payload, Stage::Migrating(operation), send)
    }
    fn operate(
        &mut self,
        context: Context,
        operation: Operation,
        state_revision: u64,
        document: Document,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let (query, cursor_query, cursor, limit, projection) = match operation {
            Operation::List { cursor, limit } => (
                String::new(),
                String::new(),
                cursor,
                limit,
                ScanProjection::List,
            ),
            Operation::ExportList { cursor, limit } => (
                String::new(),
                String::new(),
                cursor,
                limit,
                ScanProjection::ExportList,
            ),
            Operation::ExportIdentityInventory { cursor, limit } => (
                String::new(),
                String::new(),
                cursor,
                limit,
                ScanProjection::IdentityInventory,
            ),
            Operation::ProfileList { cursor, limit } => (
                String::new(),
                "remote_monitor_profiles".into(),
                cursor,
                limit,
                ScanProjection::Profiles,
            ),
            Operation::ExportSteamIds { cursor, limit } => (
                String::new(),
                "steam_ids".into(),
                cursor,
                limit,
                ScanProjection::SteamIds,
            ),
            Operation::Search {
                query,
                cursor,
                limit,
            } => {
                if !bounded_text(&query, 160) || query.trim().is_empty() {
                    return complete(
                        send,
                        context.request_id,
                        error(
                            "invalid_request",
                            "search query must contain 1..160 printable bytes",
                        ),
                    );
                }
                let normalized_query = query.to_lowercase();
                (
                    normalized_query.clone(),
                    normalized_query,
                    cursor,
                    limit,
                    ScanProjection::List,
                )
            }
            operation @ (Operation::ResolveLaunch { .. }
            | Operation::ResolveExportedLaunch { .. }) => {
                let reply = match apply(operation, Some(document)) {
                    Ok(Change::Read(reply)) => reply,
                    Ok(Change::Write { .. }) => {
                        return Err("launch resolution unexpectedly attempted a mutation".into());
                    }
                    Err(failure) => return complete(send, context.request_id, failure),
                };
                let Some(entry_id) = reply["id"].as_str() else {
                    return complete(send, context.request_id, malformed());
                };
                let entry: Entry = match serde_json::from_value::<Entry>(reply["entry"].clone()) {
                    Ok(entry) if entry.validate().is_ok() => entry,
                    _ => return complete(send, context.request_id, malformed()),
                };
                let (importer, expected_source_id) =
                    match &entry.launch {
                        Launch::Provider {
                            provider,
                            application_id,
                            ..
                        } => {
                            let Some(importer) = self.provider_launch_principals.iter().find_map(
                                |(importer, key)| (key == provider).then_some(importer.clone()),
                            ) else {
                                return complete(
                                    send,
                                    context.request_id,
                                    error(
                                        "provider_identity_required",
                                        "provider target has no configured importer",
                                    ),
                                );
                            };
                            if self
                                .import_principals
                                .get(&importer)
                                .is_none_or(|owner| context.key != format!("principal/{owner}"))
                            {
                                return complete(
                                    send,
                                    context.request_id,
                                    error(
                                        "provider_identity_required",
                                        "provider importer is not authorized for this catalog",
                                    ),
                                );
                            }
                            (importer, Some(application_id.clone()))
                        }
                        _ => (LUTRIS_IMPORTER.to_owned(), None),
                    };
                let scan = LaunchMarkerScan {
                    state_revision,
                    entry_id: entry_id.to_owned(),
                    fingerprint: provider_entry_fingerprint(&importer, &entry),
                    importer,
                    expected_source_id,
                    // State cursors name existing keys. Start at the beginning
                    // and resume only from keys returned by this revision.
                    after: None,
                    scanned: 0,
                    source_id: None,
                    matched: false,
                    reply,
                };
                return self.scan_launch_markers(context, scan, send);
            }
            operation => {
                let profile_mutation = matches!(
                    operation,
                    Operation::ProfileUpsert { .. } | Operation::ProfileDelete { .. }
                );
                return match apply(operation, Some(document)) {
                    Ok(Change::Read(reply)) => complete_selected(send, context, reply),
                    Ok(Change::Write { document, reply }) => {
                        let id = reply["id"].as_str().expect("mutation ID").to_owned();
                        let reply = if profile_mutation {
                            crate::profiles::project_reply(reply)
                        } else {
                            reply
                        };
                        let payload = json!({"action":"collection_commit","collection":context.key,"expected_revision":state_revision,"metadata":{"revision":document.revision,"next_id":document.next_id,"provider_count":context.provider_count},"edits":[{"key":id,"value":document.entries.get(&id)}]});
                        self.submit(context, payload, Stage::Saving(reply), send)
                    }
                    Err(failure) => complete(send, context.request_id, failure),
                };
            }
        };
        let limit = limit.unwrap_or(20);
        if !(1..=32).contains(&limit) {
            return complete(
                send,
                context.request_id,
                error("invalid_request", "page limit must be between 1 and 32"),
            );
        }
        if let Some(cursor) = &cursor {
            if cursor.revision != document.revision {
                return complete(
                    send,
                    context.request_id,
                    json!({"ok":false,"error":{"code":"revision_conflict","message":"catalog revision differs","current_revision":document.revision}}),
                );
            }
            if cursor.query != cursor_query || !valid_collection_cursor(&cursor.after) {
                return complete(
                    send,
                    context.request_id,
                    error(
                        "invalid_cursor",
                        "cursor does not match this query or catalog",
                    ),
                );
            }
        }
        let scan = Scan {
            projection,
            state_revision,
            revision: document.revision,
            query,
            cursor_query,
            limit,
            entries: Vec::new(),
            bytes: 0,
            scanned: 0,
            after: cursor.map(|cursor| cursor.after),
        };
        self.page(context, scan, send)
    }
    fn scan_launch_markers(
        &mut self,
        context: Context,
        scan: LaunchMarkerScan,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let request = json!({
            "action":"collection_page", "collection":context.key,
            "revision":scan.state_revision, "after":scan.after, "limit":128,
        });
        self.submit(context, request, Stage::LaunchMarkerPage(scan), send)
    }

    fn consume_launch_marker_page(
        &mut self,
        context: Context,
        mut scan: LaunchMarkerScan,
        state_revision: u64,
        payload: Value,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if state_revision != scan.state_revision || payload["revision"] != scan.state_revision {
            return complete(send, context.request_id, malformed());
        }
        let Some(rows) = payload["entries"].as_array() else {
            return complete(send, context.request_id, malformed());
        };
        let prefix = format!("{ENTRY_PREFIX}{}:", scan.importer);
        let mut past_prefix = false;
        for row in rows {
            let Some(key) = row["key"].as_str() else {
                return complete(send, context.request_id, malformed());
            };
            if scan.after.as_deref().is_some_and(|after| key <= after) {
                return complete(send, context.request_id, malformed());
            }
            scan.after = Some(key.to_owned());
            scan.scanned += 1;
            if scan.scanned > MAX_LAUNCH_MARKER_SCAN {
                return complete(send, context.request_id, malformed());
            }
            if !key.starts_with(&prefix) {
                if key < prefix.as_str() {
                    continue;
                }
                past_prefix = true;
                break;
            }
            let source_id = &key[prefix.len()..];
            let marker: Marker = match serde_json::from_value(row["value"].clone()) {
                Ok(marker) if validate_marker(&marker, &scan.importer, source_id) => marker,
                _ => return complete(send, context.request_id, malformed()),
            };
            if marker.entry_id == scan.entry_id {
                if scan.matched {
                    return complete(send, context.request_id, malformed());
                }
                scan.matched = true;
                if let Some(expected_source_id) = &scan.expected_source_id {
                    if source_id != expected_source_id {
                        return complete(send, context.request_id, malformed());
                    }
                } else {
                    let numeric_id = source_id
                        .parse::<i64>()
                        .ok()
                        .filter(|id| *id > 0 && id.to_string() == source_id);
                    if numeric_id.is_none() {
                        return complete(send, context.request_id, malformed());
                    }
                }
                if !marker.detached && marker.fingerprint == scan.fingerprint {
                    scan.source_id = Some(source_id.to_owned());
                }
            }
        }
        let more = match payload.get("next") {
            Some(Value::Null) => false,
            Some(Value::String(_)) if past_prefix => false,
            Some(Value::String(next))
                if Some(next.as_str()) == scan.after.as_deref() && !rows.is_empty() =>
            {
                true
            }
            _ => return complete(send, context.request_id, malformed()),
        };
        if more && rows.is_empty() {
            return complete(send, context.request_id, malformed());
        }
        if !past_prefix && more {
            if scan.scanned >= MAX_LAUNCH_MARKER_SCAN {
                return complete(send, context.request_id, malformed());
            }
            return self.scan_launch_markers(context, scan, send);
        }
        if let Some(source_id) = scan.source_id {
            scan.reply["provider_provenance"] =
                json!({"provider":scan.importer,"source_id":source_id});
        } else if scan.expected_source_id.is_some() {
            return complete(
                send,
                context.request_id,
                error(
                    "provider_identity_required",
                    "provider target has no attached importer marker",
                ),
            );
        }
        complete_selected(send, context, scan.reply)
    }

    fn page(
        &mut self,
        context: Context,
        scan: Scan,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let limit = 32.min(MAX_SCAN - scan.scanned);
        let payload = json!({
            "action": "collection_page", "collection": context.key,
            "revision": scan.state_revision, "after": scan.after, "limit": limit,
        });
        self.submit(context, payload, Stage::Paging(scan), send)
    }
    fn consume_page(
        &mut self,
        context: Context,
        mut scan: Scan,
        payload: Value,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if payload["revision"] != scan.state_revision {
            return complete(send, context.request_id, malformed());
        }
        let Some(entries) = payload["entries"].as_array() else {
            return complete(send, context.request_id, malformed());
        };
        for row in entries {
            let Some(id) = row["key"].as_str() else {
                return complete(send, context.request_id, malformed());
            };
            if is_reserved_key(id) {
                // Public application IDs sort before every reserved key. Once
                // this boundary is reached there can be no further public
                // entries; never expose a hidden provider key as a cursor.
                return complete(send, context.request_id, scan.reply(false));
            }
            let entry: Entry = match serde_json::from_value(row["value"].clone()) {
                Ok(entry) => entry,
                Err(_) => return complete(send, context.request_id, malformed()),
            };
            if crate::model::parse_id(id).is_none()
                || entry.validate().is_err()
                || scan.after.as_deref().is_some_and(|after| id <= after)
            {
                return complete(send, context.request_id, malformed());
            }
            let projected_row = match scan.projection {
                ScanProjection::List if matches_query(&entry, &scan.query) => {
                    Some(project_entry(id, &entry, false))
                }
                ScanProjection::ExportList
                    if !entry.hidden && matches_query(&entry, &scan.query) =>
                {
                    Some(project_entry(id, &entry, true))
                }
                ScanProjection::IdentityInventory => match crate::identity::project(id, &entry) {
                    Ok(projected) => Some(projected),
                    Err(message) => {
                        return complete(send, context.request_id, error("invalid_entry", message));
                    }
                },
                ScanProjection::Profiles if entry.remote_monitor_profile => {
                    match crate::identity::project(id, &entry) {
                        Ok(projected) => Some(projected),
                        Err(message) => {
                            return complete(
                                send,
                                context.request_id,
                                error("invalid_profile", message),
                            );
                        }
                    }
                }
                ScanProjection::SteamIds => entry
                    .steam_id
                    .as_deref()
                    .filter(|steam_id| crate::model::valid_steam_id(steam_id))
                    .and_then(|steam_id| steam_id.parse::<u32>().ok())
                    .map(Value::from),
                _ => None,
            };
            if let Some(row) = projected_row {
                let size = serde_json::to_vec(&row).expect("JSON value").len();
                if scan.entries.len() >= scan.limit || scan.bytes + size > PAGE_BYTES {
                    return complete(send, context.request_id, scan.reply(true));
                }
                scan.bytes += size;
                scan.entries.push(row);
            }
            scan.after = Some(id.into());
            scan.scanned += 1;
        }
        let more = payload["next"].is_string();
        if more && entries.is_empty() {
            return complete(send, context.request_id, malformed());
        }
        if !more || scan.scanned >= MAX_SCAN {
            complete(send, context.request_id, scan.reply(more))
        } else {
            self.page(context, scan, send)
        }
    }
}
impl Scan {
    fn reply(self, more: bool) -> Value {
        let cursor = if more {
            self.after.map(|after| Cursor {
                revision: self.revision,
                after,
                query: self.cursor_query,
            })
        } else {
            None
        };
        match self.projection {
            ScanProjection::SteamIds => {
                json!({"ok":true,"revision":self.revision,"steam_ids":self.entries,"next_cursor":cursor})
            }
            ScanProjection::List => {
                let mut reply = json!({"ok":true,"revision":self.revision,"entries":self.entries,"next_cursor":cursor});
                reply["config_override_capabilities"] =
                    crate::protocol::config_override_capabilities();
                reply
            }
            ScanProjection::ExportList
            | ScanProjection::IdentityInventory
            | ScanProjection::Profiles => {
                json!({"ok":true,"revision":self.revision,"entries":self.entries,"next_cursor":cursor})
            }
        }
    }
}
fn complete_read(
    send: &mut impl FnMut(Command) -> TransportResult,
    context: Context,
    operation: Operation,
    document: Document,
) -> ModuleResult<()> {
    let reply = match apply(operation, Some(document)) {
        Ok(Change::Read(reply)) | Err(reply) => reply,
        Ok(Change::Write { .. }) => return Err("export unexpectedly attempted a mutation".into()),
    };
    complete_selected(send, context, reply)
}

fn complete_selected(
    send: &mut impl FnMut(Command) -> TransportResult,
    context: Context,
    mut reply: Value,
) -> ModuleResult<()> {
    if reply["ok"] == true
        && let Some(owner) = context.catalog_owner
    {
        reply["catalog_owner"] = owner.into();
    }
    complete(send, context.request_id, reply)
}

pub(crate) fn header(metadata: &Value) -> Result<Document, Value> {
    let mut value = metadata.clone();
    if !value.is_object() {
        return Err(malformed());
    }
    value
        .as_object_mut()
        .expect("metadata object")
        .remove("provider_count");
    value["entries"] = json!({});
    let document: Document = serde_json::from_value(value).map_err(|_| malformed())?;
    document.validate().map_err(|_| malformed())?;
    Ok(document)
}

/// Bound cleanup work while leaving the current generation and public entries untouched.
fn reclaim_abandoned_batches(session: &mut ProviderSession) -> Vec<Value> {
    let boundary = staged_key(&session.caller, session.state.generation, 0);
    // Rows in this map belong to this caller, and fixed-width generations sort
    // in chronological order. The range avoids rescanning the active snapshot.
    let keys: Vec<String> = session
        .staged_rows
        .range(..boundary)
        .take(4) // Reserve frame space for a maximum-sized incoming batch.
        .map(|(key, _)| key.clone())
        .collect();
    for key in &keys {
        session.staged_rows.remove(key);
        session.provider_count -= 1;
    }
    keys.into_iter()
        .map(|key| json!({"key":key,"value":null}))
        .collect()
}

fn staged_key(caller: &str, generation: u64, sequence: u64) -> String {
    format!("{STAGED_PREFIX}{caller}:{generation:020}:{sequence:020}")
}

fn parse_staged_key(key: &str) -> Option<(&str, u64, u64)> {
    let rest = key.strip_prefix(STAGED_PREFIX)?;
    let (rest, sequence) = rest.rsplit_once(':')?;
    let (caller, generation) = rest.rsplit_once(':')?;
    if caller.is_empty() || generation.len() != 20 || sequence.len() != 20 {
        return None;
    }
    let generation = generation.parse().ok()?;
    let sequence = sequence.parse().ok()?;
    (staged_key(caller, generation, sequence) == key).then_some((caller, generation, sequence))
}

fn extend_intents(target: &mut StagedIntents, source: &StagedIntents) {
    target.entries.extend(source.entries.clone());
    target
        .remove_source_ids
        .extend(source.remove_source_ids.clone());
    target
        .expire_source_ids
        .extend(source.expire_source_ids.clone());
    target
        .selection_remove_source_ids
        .extend(source.selection_remove_source_ids.clone());
}

fn collect_staged(session: &ProviderSession) -> Result<StagedIntents, Value> {
    let mut result = session.state.staged_intents.clone();
    let mut sequence = if result.is_empty() { Some(0) } else { None };
    let mut found = false;
    for (key, row) in &session.staged_rows {
        let (_, generation, row_sequence) = parse_staged_key(key).ok_or_else(malformed)?;
        if generation == session.state.generation {
            if sequence.is_some_and(|expected| row_sequence != expected) {
                return Err(malformed());
            }
            sequence = row_sequence.checked_add(1);
            if sequence.is_none() {
                return Err(malformed());
            }
            found = true;
            extend_intents(&mut result, row);
        }
    }
    // Old in-state staged snapshots remain readable while upgrading.
    if (found && sequence != Some(session.state.next_sequence))
        || (!found && result.is_empty() && session.state.next_sequence != 0)
    {
        return Err(malformed());
    }
    Ok(result)
}

fn validate_staged_intents(
    caller: &str,
    provider_key: Option<&str>,
    seen: &BTreeSet<String>,
    intents: &StagedIntents,
) -> Result<BTreeSet<String>, Value> {
    let mut ids = BTreeSet::new();
    for source_id in intents
        .entries
        .iter()
        .map(|entry| entry.source_id.as_str())
        .chain(intents.remove_source_ids.iter().map(String::as_str))
        .chain(intents.expire_source_ids.iter().map(String::as_str))
        .chain(
            intents
                .selection_remove_source_ids
                .iter()
                .map(String::as_str),
        )
    {
        if !valid_source_id(source_id)
            || (caller == PLAYNITE_IMPORTER && source_id == PLAYNITE_FULLSCREEN_SOURCE_ID)
            || seen.contains(source_id)
            || !ids.insert(source_id.to_owned())
        {
            return Err(error(
                "duplicate_source_id",
                "provider snapshot repeats a source ID within its generation",
            ));
        }
    }
    for item in &intents.entries {
        validate_provider_entry(
            caller,
            provider_key,
            &item.source_id,
            &item.entry,
            item.preserve_artwork,
        )?;
    }
    Ok(ids)
}

fn staged_batch(intents: &StagedIntents, revision: u64) -> ProviderBatch {
    ProviderBatch {
        sequence: 0,
        expected_revision: revision,
        fingerprint: String::new(),
        entries: intents.entries.clone(),
        remove_source_ids: intents.remove_source_ids.clone(),
        expire_source_ids: intents.expire_source_ids.clone(),
        selection_remove_source_ids: intents.selection_remove_source_ids.clone(),
    }
}

/// Validate and plan a provider batch without publishing the collection edits.
fn validate_provider_entry(
    caller: &str,
    provider_key: Option<&str>,
    source_id: &str,
    entry: &Entry,
    preserve_artwork: bool,
) -> Result<(), Value> {
    let invalid_generic_provider = match &entry.launch {
        Launch::Provider {
            provider,
            application_id,
            ..
        } => provider_key != Some(provider.as_str()) || application_id != source_id,
        _ => false,
    };
    let invalid_launch_owner = matches!(
        &entry.launch,
        Launch::Playnite { .. } | Launch::PlayniteFullscreen { .. }
    ) && caller != PLAYNITE_IMPORTER;
    // Fullscreen is a reserved provider row and Desktop is user-managed;
    // neither can arrive through a library snapshot.
    let invalid_fullscreen_import = matches!(
        &entry.launch,
        Launch::PlayniteFullscreen { .. } | Launch::Desktop { .. }
    );
    let valid_playnite_identity = caller == PLAYNITE_IMPORTER
        && crate::model::valid_guid(source_id)
        && !source_id.bytes().any(|byte| byte.is_ascii_uppercase())
        && matches!(&entry.launch, Launch::Playnite { game_id, .. } if game_id.as_str() == source_id);
    let invalid_artwork_owner = match &entry.artwork {
        Some(ArtworkReference::Steam { app_id, .. }) => {
            caller != STEAM_IMPORTER || source_id != format!("steam:{app_id}")
        }
        Some(ArtworkReference::Lutris { game_id, .. }) => {
            caller != LUTRIS_IMPORTER || source_id != game_id.to_string()
        }
        Some(ArtworkReference::Playnite { game_id, .. }) => {
            !valid_playnite_identity || game_id.as_str() != source_id
        }
        Some(ArtworkReference::External { source, .. }) => {
            provider_key != Some(source.as_str())
                || !matches!(&entry.launch, Launch::Provider { provider, application_id, .. }
                    if provider == source && application_id == source_id)
        }
        None => false,
    } || (preserve_artwork
        && (!valid_playnite_identity || entry.artwork.is_some()));
    let has_steam_metadata = entry.steam_id.is_some() || entry.steam_install_dir.is_some();
    let valid_steam_metadata_identity = entry
        .steam_id
        .as_ref()
        .is_some_and(|app_id| caller == STEAM_IMPORTER && source_id == format!("steam:{app_id}"));
    let invalid_steam_metadata = has_steam_metadata && !valid_steam_metadata_identity;
    if invalid_artwork_owner {
        return Err(error(
            "provider_artwork_required",
            "provider artwork requires its authenticated importer and matching source ID",
        ));
    }
    if invalid_steam_metadata {
        return Err(error(
            "provider_identity_required",
            "Steam metadata requires the authenticated Steam importer and matching source ID",
        ));
    }
    if entry.validate().is_err()
        || invalid_launch_owner
        || invalid_fullscreen_import
        || invalid_generic_provider
    {
        return Err(error(
            "invalid_entry",
            "provider entry identity or value is invalid",
        ));
    }
    Ok(())
}

fn plan_provider_batch(
    mut session: ProviderSession,
    batch: ProviderBatch,
) -> Result<(ProviderSession, Vec<Value>), Value> {
    let ProviderBatch {
        sequence,
        expected_revision,
        fingerprint: batch_fingerprint,
        entries,
        remove_source_ids,
        expire_source_ids,
        selection_remove_source_ids,
    } = batch;
    let mut source_ids = BTreeSet::new();
    for source_id in entries
        .iter()
        .map(|entry| entry.source_id.as_str())
        .chain(remove_source_ids.iter().map(String::as_str))
        .chain(expire_source_ids.iter().map(String::as_str))
        .chain(selection_remove_source_ids.iter().map(String::as_str))
    {
        if !valid_source_id(source_id)
            || (session.caller == PLAYNITE_IMPORTER && source_id == PLAYNITE_FULLSCREEN_SOURCE_ID)
            || session.seen_source_ids.contains(source_id)
            || !source_ids.insert(source_id.to_owned())
        {
            return Err(error(
                "duplicate_source_id",
                "provider snapshot repeats a source ID within its generation",
            ));
        }
    }
    let mut owned_entry_ids = BTreeSet::new();
    if session
        .markers
        .values()
        .any(|marker| !owned_entry_ids.insert(marker.entry_id.clone()))
    {
        return Err(malformed());
    }
    let mut edits = Vec::new();
    let mut entries_changed = false;
    for provider_entry in entries {
        let ProviderEntry {
            source_id,
            mut entry,
            source_flags,
            preserve_artwork,
        } = provider_entry;
        validate_provider_entry(
            &session.caller,
            session.provider_key.as_deref(),
            &source_id,
            &entry,
            preserve_artwork,
        )?;
        let key = entry_key(&session.caller, &source_id);
        let existing = session.markers.get(&key).cloned();
        let source_flags = source_flags
            .or_else(|| existing.as_ref().map(|marker| marker.source_flags))
            .unwrap_or_default();
        let added_at_unix = match existing.as_ref() {
            Some(marker) => marker.added_at_unix,
            None if session.state.staged => session.state.staged_at_unix,
            None => unix_timestamp(),
        };
        let (entry_id, detached, config_overrides) = if let Some(marker) = existing.clone() {
            if !validate_marker(&marker, &session.caller, &source_id) {
                return Err(malformed());
            }
            if marker.detached
                || session.fingerprints.get(&marker.entry_id) != Some(&marker.fingerprint)
            {
                (marker.entry_id, true, marker.config_overrides)
            } else {
                (marker.entry_id, false, marker.config_overrides)
            }
        } else {
            if session.entry_ids.len() >= MAX_ENTRIES {
                return Err(error("resource_exhausted", "catalog entry limit reached"));
            }
            let entry_id = format!("app-{:016x}", session.next_id);
            if session.entry_ids.contains(&entry_id) || owned_entry_ids.contains(&entry_id) {
                return Err(error("entry_conflict", "next catalog ID is already owned"));
            }
            let Some(next_id) = session.next_id.checked_add(1) else {
                return Err(error("resource_exhausted", "catalog ID space exhausted"));
            };
            session.next_id = next_id;
            session.entry_ids.insert(entry_id.clone());
            owned_entry_ids.insert(entry_id.clone());
            (entry_id, false, ConfigOverrides::default())
        };
        if preserve_artwork
            && !detached
            && let Some(previous) = session.artwork_entries.get(&entry_id)
            && matches!((&previous.launch, &previous.artwork),
                (Launch::Playnite { game_id: launch_id, .. }, Some(ArtworkReference::Playnite { game_id, .. }))
                    if launch_id == &source_id && game_id == &source_id)
        {
            entry.artwork = previous.artwork.clone();
        }
        if session.caller == PLAYNITE_IMPORTER {
            entry.config_overrides = config_overrides.clone();
        }
        let desired_fingerprint = provider_entry_fingerprint(&session.caller, &entry);
        let mut marker = Marker {
            version: 1,
            caller: session.caller.clone(),
            source_id: source_id.clone(),
            entry_id: entry_id.clone(),
            generation: session.state.generation,
            fingerprint: desired_fingerprint.clone(),
            config_overrides,
            detached,
            remove_generation: None,
            added_at_unix,
            source_flags,
            remove_reason: None,
        };
        if detached {
            // A changed or deleted Entry has become user-owned. Keep it and
            // stop provider updates from silently replacing it.
            marker.fingerprint = session
                .fingerprints
                .get(&entry_id)
                .cloned()
                .unwrap_or_else(|| "0".repeat(64));
        } else {
            if session.fingerprints.get(&entry_id) != Some(&desired_fingerprint) {
                session
                    .fingerprints
                    .insert(entry_id.clone(), desired_fingerprint);
                edits.push(json!({"key":entry_id,"value":entry}));
                entries_changed = true;
            }
            session.entry_ids.insert(entry_id.clone());
        }
        if !session.markers.contains_key(&key) {
            session.provider_count = session.provider_count.saturating_add(1);
        }
        session.markers.insert(key.clone(), marker.clone());
        session.seen_source_ids.insert(source_id);
        edits.push(json!({"key":key,"value":marker_value(&marker)}));
    }
    for (source_id, reason) in remove_source_ids
        .into_iter()
        .map(|source_id| (source_id, RemovalReason::Uninstalled))
        .chain(
            expire_source_ids
                .into_iter()
                .map(|source_id| (source_id, RemovalReason::Expired)),
        )
        .chain(
            selection_remove_source_ids
                .into_iter()
                .map(|source_id| (source_id, RemovalReason::Selection)),
        )
    {
        let key = entry_key(&session.caller, &source_id);
        if let Some(mut marker) = session.markers.get(&key).cloned() {
            if !validate_marker(&marker, &session.caller, &source_id) {
                return Err(malformed());
            }
            marker.remove_generation = Some(session.state.generation);
            marker.remove_reason = Some(reason);
            session.markers.insert(key.clone(), marker.clone());
            edits.push(json!({"key":key,"value":marker_value(&marker)}));
        }
        session.seen_source_ids.insert(source_id);
    }
    let new_revision = if entries_changed {
        let Some(revision) = session.document_revision.checked_add(1) else {
            return Err(error("resource_exhausted", "catalog revision exhausted"));
        };
        revision
    } else {
        session.document_revision
    };
    let Some(next_sequence) = session.state.next_sequence.checked_add(1) else {
        return Err(error(
            "resource_exhausted",
            "provider sequence space exhausted",
        ));
    };
    session.state.next_sequence = next_sequence;
    session.state.finished = false;
    session.state.cleanup_pending = false;
    session.state.finish_expected_revision = None;
    session.state.finish_remove_stale = true;
    session.state.finish_remove_uninstalled = false;
    session.state.finish_remove_expired = false;
    session.state.finish_remove_selection = false;
    session.state.last_batch = Some(CommittedBatch {
        sequence,
        expected_revision,
        revision: new_revision,
        next_sequence,
        fingerprint: batch_fingerprint,
    });
    session.document_revision = new_revision;
    edits.push(json!({
        "key":state_key(&session.caller),
        "value":session.state.clone(),
    }));
    Ok((session, edits))
}

/// Select cleanup candidates without changing the collection. A staged publisher
/// can combine these candidates with its planned writes in one transaction.
fn plan_stale_markers(
    session: &ProviderSession,
    generation: u64,
    remove_stale: bool,
    remove_uninstalled: bool,
    remove_expired: bool,
    remove_selection: bool,
) -> Vec<Marker> {
    session
        .markers
        .values()
        .filter(|marker| {
            marker.caller == session.caller
                && !(session.caller == PLAYNITE_IMPORTER
                    && marker.source_id == PLAYNITE_FULLSCREEN_SOURCE_ID)
                && ((remove_stale && marker.generation < generation)
                    || (marker.remove_generation == Some(generation)
                        && match marker.remove_reason {
                            Some(RemovalReason::Uninstalled) | None => remove_uninstalled,
                            Some(RemovalReason::Expired) => remove_expired,
                            Some(RemovalReason::Selection) => remove_selection,
                        }))
        })
        .cloned()
        .collect()
}

/// Plan cleanup against the scanned collection, preserving edited or detached
/// entries. This only mutates the in-memory session; the caller publishes edits.
fn plan_cleanup_edits(
    session: &mut ProviderSession,
    stale: Vec<Marker>,
) -> Result<Vec<Value>, Value> {
    let mut edits = Vec::with_capacity(stale.len() * 2);
    let mut removed_entry = false;
    for marker in stale {
        let key = entry_key(&session.caller, &marker.source_id);
        if !marker.detached
            && session.fingerprints.get(&marker.entry_id) == Some(&marker.fingerprint)
        {
            edits.push(json!({"key":marker.entry_id,"value":null}));
            session.entry_ids.remove(&marker.entry_id);
            session.fingerprints.remove(&marker.entry_id);
            removed_entry = true;
        }
        edits.push(json!({"key":key,"value":null}));
        session.markers.remove(&key);
        session.provider_count = session.provider_count.saturating_sub(1);
    }
    if removed_entry {
        session.document_revision = session
            .document_revision
            .checked_add(1)
            .ok_or_else(|| error("resource_exhausted", "catalog revision exhausted"))?;
    }
    Ok(edits)
}

fn collection_commit(session: &ProviderSession, edits: Vec<Value>) -> Value {
    json!({
        "action":"collection_commit",
        "collection":session.key,
        "expected_revision":session.collection_revision,
        "metadata":{
            "revision":session.document_revision,
            "next_id":session.next_id,
            "provider_count":session.provider_count,
        },
        "edits":edits,
    })
}

fn playnite_fullscreen_entry(config_overrides: ConfigOverrides) -> Entry {
    Entry {
        name: PLAYNITE_FULLSCREEN_NAME.into(),
        uuid: None,
        id_aliases: Vec::new(),
        hidden: false,
        remote_monitor_profile: false,
        properties: json!({}),
        description: String::new(),
        tags: Vec::new(),
        artwork: None,
        steam_id: None,
        steam_install_dir: None,
        output: None,
        display_output: None,
        virtual_screen: None,
        virtual_display_mode: None,
        virtual_display_layout: None,
        config_overrides,
        exclude_global_prep_cmd: false,
        prep_cmd: Vec::new(),
        elevated: false,
        launch: Launch::PlayniteFullscreen {
            exit_timeout_seconds: 10,
            working_directory: None,
            preparation: Vec::new(),
        },
    }
}

fn session_entry_id_available(session: &ProviderSession, entry_id: &str) -> bool {
    crate::model::parse_id(entry_id).is_some_and(|number| number < session.next_id)
        && !session.markers.iter().any(|(key, marker)| {
            marker.entry_id == entry_id
                && key != &entry_key(PLAYNITE_IMPORTER, PLAYNITE_FULLSCREEN_SOURCE_ID)
        })
}

fn snapshot_matches(
    session: &ProviderSession,
    sync_id: &str,
    generation: u64,
    source_revision: u64,
    provider_epoch: u64,
) -> bool {
    !session.state.revoked
        && session.state.provider_epoch == provider_epoch
        && session.state.generation == generation
        && session.state.sync_id == sync_id
        && session.state.source_revision == source_revision
}

fn revision_conflict(current_revision: u64) -> Value {
    json!({"ok":false,"error":{"code":"revision_conflict","message":"catalog revision differs","current_revision":current_revision}})
}

fn unix_timestamp() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .filter(|timestamp| *timestamp <= 253_402_300_799)
}

fn valid_collection_cursor(value: &str) -> bool {
    crate::model::parse_id(value).is_some() || is_reserved_key(value)
}

/// Hidden rows that are never catalog entries: provider markers and state, and
/// the first-run seed marker.
fn is_reserved_key(key: &str) -> bool {
    key.starts_with(ENTRY_PREFIX)
        || key.starts_with(STATE_PREFIX)
        || key.starts_with(STAGED_PREFIX)
        || key == seed::MARKER_KEY
}
fn malformed() -> Value {
    error(
        "invalid_state_response",
        "persisted catalog or state response is malformed",
    )
}
fn runtime_failure(response: Response) -> Value {
    match response {
        Response::Error { code, message } => error(&code, message),
        _ => malformed(),
    }
}
fn complete(
    send: &mut impl FnMut(Command) -> TransportResult,
    request: u64,
    payload: Value,
) -> ModuleResult<()> {
    match send(Command::Complete { request, payload })? {
        Response::Ok => Ok(()),
        Response::Error { code, .. } if code == "request_ended" || code == "not_found" => Ok(()),
        unexpected => Err(format!("catalog completion failed: {unexpected:?}")),
    }
}
#[cfg(test)]
mod tests;
