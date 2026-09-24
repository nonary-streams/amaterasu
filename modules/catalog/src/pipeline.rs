use crate::STATE_SERVICE;
use crate::model::{Document, Entry, MAX_ENTRIES, bounded_text};
use crate::product::{
    CommittedBatch, ENTRY_PREFIX, MAX_BATCH_BYTES, MAX_BATCH_ENTRIES, Marker, STATE_PREFIX,
    SnapshotState, entry_fingerprint, entry_key, fingerprint_bytes, marker_value, parse_entry_key,
    state_key, valid_source_id, valid_sync_id, validate_marker,
};
use crate::protocol::{
    Change, Cursor, Operation, ProviderEntry, apply, error, matches_query, project_entry,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

const MAX_QUEUED: usize = 32;
const PAGE_BYTES: usize = 12_000;
const MAX_SCAN: usize = 256;
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
    Loading(Operation),
    Legacy(Operation),
    Migrating(Operation),
    Paging(Scan),
    Saving(Value),
    ProviderLoading(Operation),
    ProviderPaging(ProviderScan),
    ProviderSaving(ProviderSave),
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
    entry_ids: BTreeSet<String>,
    markers: BTreeMap<String, Marker>,
    provider_state: Option<SnapshotState>,
}

#[derive(Clone)]
struct ProviderSession {
    caller: String,
    key: String,
    collection_revision: u64,
    document_revision: u64,
    next_id: u64,
    provider_count: u64,
    entry_ids: BTreeSet<String>,
    fingerprints: BTreeMap<String, String>,
    markers: BTreeMap<String, Marker>,
    state: SnapshotState,
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
}
struct ProviderFinish {
    sync_id: String,
    generation: u64,
    source_revision: u64,
    expected_revision: u64,
    next_sequence: u64,
    source_complete: bool,
    remove_stale: bool,
}
struct Scan {
    export: bool,
    state_revision: u64,
    revision: u64,
    query: String,
    limit: usize,
    entries: Vec<Value>,
    bytes: usize,
    scanned: usize,
    after: Option<String>,
}
#[derive(Default)]
pub struct Pipeline {
    launch_principals: BTreeSet<String>,
    export_principals: BTreeMap<String, String>,
    import_principals: BTreeMap<String, String>,
    provider_sessions: BTreeMap<String, ProviderSession>,
    queued: VecDeque<Incoming>,
    pending: Option<Pending>,
    sequence: u64,
}
impl Pipeline {
    pub fn new(
        launch_principals: BTreeSet<String>,
        export_principals: BTreeMap<String, String>,
        import_principals: BTreeMap<String, String>,
    ) -> Self {
        Self {
            launch_principals,
            export_principals,
            import_principals,
            ..Self::default()
        }
    }

    fn select_catalog(
        &self,
        caller: &str,
        initiator: &str,
        operation: Operation,
    ) -> Result<(String, Option<String>, Operation), Value> {
        match operation {
            operation @ (Operation::ProviderSnapshotBegin { .. }
            | Operation::ProviderSnapshotBatch { .. }
            | Operation::ProviderSnapshotFinish { .. }) => {
                let owner = self.import_principals.get(caller).ok_or_else(|| {
                    error(
                        "permission_denied",
                        "caller is not an authorized catalog importer",
                    )
                })?;
                Ok((format!("principal/{owner}"), None, operation))
            }
            operation @ Operation::ExportList { .. } => {
                let owner = self.export_principals.get(caller).ok_or_else(|| {
                    error(
                        "permission_denied",
                        "caller is not an authorized catalog consumer",
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
                    Operation::Get { id },
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
        if self.pending.is_none()
            && let Some(incoming) = self.queued.pop_front()
        {
            let context = Context {
                request_id: incoming.request_id,
                key: incoming.key,
                catalog_owner: incoming.catalog_owner,
                importer: match &incoming.operation {
                    Operation::ProviderSnapshotBegin { .. }
                    | Operation::ProviderSnapshotBatch { .. }
                    | Operation::ProviderSnapshotFinish { .. } => Some(incoming.caller.clone()),
                    _ => None,
                },
                provider_count: 0,
                deadline: incoming.deadline,
            };
            // The importer is the Poll caller, retained separately from the fixed
            // owner collection key. It cannot be supplied by request payload.
            if matches!(
                incoming.operation,
                Operation::ProviderSnapshotBegin { .. }
                    | Operation::ProviderSnapshotBatch { .. }
                    | Operation::ProviderSnapshotFinish { .. }
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
                self.queued.push_back(Incoming {
                    request_id: id,
                    caller,
                    key,
                    catalog_owner,
                    operation,
                    deadline: Instant::now() + Duration::from_millis(remaining_ms.min(30_000)),
                });
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
            | Operation::ResolveExportedLaunch { id }
            | Operation::Update { id, .. }
            | Operation::Delete { id, .. } => Some(id),
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
        if !matches!(operation, Operation::ProviderSnapshotBegin { .. })
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
        let state = scan.provider_state.unwrap_or_else(SnapshotState::initial);
        if !state.validate() {
            return complete(send, context.request_id, malformed());
        }
        let session = ProviderSession {
            caller,
            key: context.key.clone(),
            collection_revision: scan.state_revision,
            document_revision: scan.document_revision,
            next_id: scan.next_id,
            provider_count: scan.provider_count,
            entry_ids: scan.entry_ids,
            fingerprints: scan.fingerprints,
            markers: scan.markers,
            state,
        };
        self.apply_provider_operation(context, scan.operation, session, send)
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
            } => self.begin_snapshot(context, session, sync_id, source_revision, send),
            Operation::ProviderSnapshotBatch {
                sync_id,
                generation,
                source_revision,
                sequence,
                expected_revision,
                entries,
            } => {
                let encoded = serde_json::to_vec(&entries).map_or(usize::MAX, |bytes| bytes.len());
                if !valid_sync_id(&sync_id)
                    || entries.is_empty()
                    || entries.len() > MAX_BATCH_ENTRIES
                    || encoded > MAX_BATCH_BYTES
                {
                    return complete(
                        send,
                        context.request_id,
                        error("invalid_request", "provider batch exceeds its bounds"),
                    );
                }
                let batch_fingerprint = serde_json::to_vec(&entries)
                    .map(|bytes| fingerprint_bytes(&bytes))
                    .unwrap_or_default();
                if !snapshot_matches(&session, &sync_id, generation, source_revision) {
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
                            json!({"ok":true,"revision":last.revision,"generation":generation,"next_sequence":last.next_sequence,"replayed":true}),
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
                self.commit_provider_batch(
                    context,
                    session,
                    ProviderBatch {
                        sequence,
                        expected_revision,
                        fingerprint: batch_fingerprint,
                        entries,
                    },
                    send,
                )
            }
            Operation::ProviderSnapshotFinish {
                sync_id,
                generation,
                source_revision,
                expected_revision,
                next_sequence,
                complete: source_complete,
                remove_stale,
            } => self.finish_snapshot(
                context,
                session,
                ProviderFinish {
                    sync_id,
                    generation,
                    source_revision,
                    expected_revision,
                    next_sequence,
                    source_complete,
                    remove_stale,
                },
                send,
            ),
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

    fn begin_snapshot(
        &mut self,
        context: Context,
        mut session: ProviderSession,
        sync_id: String,
        source_revision: u64,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
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
            if session.state.source_revision != source_revision {
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
                json!({"ok":true,"revision":session.document_revision,"generation":session.state.generation,"next_sequence":session.state.next_sequence}),
            );
        }
        let is_new_state = session.state.generation == 0;
        let Some(generation) = session.state.generation.checked_add(1) else {
            return complete(
                send,
                context.request_id,
                error("resource_exhausted", "provider generation space exhausted"),
            );
        };
        session.state = SnapshotState {
            version: 1,
            generation,
            sync_id,
            source_revision,
            next_sequence: 0,
            last_batch: None,
            finished: false,
            cleanup_pending: false,
            finish_expected_revision: None,
            finish_remove_stale: true,
        };
        let key = state_key(&session.caller);
        if is_new_state {
            session.provider_count = session.provider_count.saturating_add(1);
        }
        let reply = json!({"ok":true,"revision":session.document_revision,"generation":generation,"next_sequence":0});
        let payload = collection_commit(
            &session,
            vec![json!({"key":key,"value":session.state.clone()})],
        );
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
        mut session: ProviderSession,
        batch: ProviderBatch,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let ProviderBatch {
            sequence,
            expected_revision,
            fingerprint: batch_fingerprint,
            entries,
        } = batch;
        let mut source_ids = BTreeSet::new();
        let mut edits = Vec::new();
        let mut entries_changed = false;
        for provider_entry in entries {
            let source_id = provider_entry.source_id;
            let entry = provider_entry.entry;
            if !valid_source_id(&source_id)
                || !source_ids.insert(source_id.clone())
                || entry.validate().is_err()
            {
                return complete(
                    send,
                    context.request_id,
                    error(
                        "invalid_entry",
                        "provider entry identity or value is invalid",
                    ),
                );
            }
            let key = entry_key(&session.caller, &source_id);
            let desired_fingerprint = entry_fingerprint(&entry);
            let existing = session.markers.get(&key).cloned();
            let (entry_id, detached) = if let Some(marker) = existing {
                if !validate_marker(&marker, &session.caller, &source_id) {
                    return complete(send, context.request_id, malformed());
                }
                if session.markers.values().any(|other| {
                    other.entry_id == marker.entry_id
                        && (other.caller != marker.caller || other.source_id != marker.source_id)
                }) {
                    return complete(send, context.request_id, malformed());
                }
                if marker.detached
                    || session.fingerprints.get(&marker.entry_id) != Some(&marker.fingerprint)
                {
                    (marker.entry_id, true)
                } else {
                    (marker.entry_id, false)
                }
            } else {
                if session.entry_ids.len() >= MAX_ENTRIES {
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
                session.next_id = next_id;
                session.entry_ids.insert(entry_id.clone());
                (entry_id, false)
            };
            let mut marker = Marker {
                version: 1,
                caller: session.caller.clone(),
                source_id: source_id.clone(),
                entry_id: entry_id.clone(),
                generation: session.state.generation,
                fingerprint: desired_fingerprint.clone(),
                detached,
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
            edits.push(json!({"key":key,"value":marker_value(&marker)}));
        }
        let new_revision = if entries_changed {
            let Some(revision) = session.document_revision.checked_add(1) else {
                return complete(
                    send,
                    context.request_id,
                    error("resource_exhausted", "catalog revision exhausted"),
                );
            };
            revision
        } else {
            session.document_revision
        };
        let Some(next_sequence) = session.state.next_sequence.checked_add(1) else {
            return complete(
                send,
                context.request_id,
                error("resource_exhausted", "provider sequence space exhausted"),
            );
        };
        session.state.next_sequence = next_sequence;
        session.state.finished = false;
        session.state.cleanup_pending = false;
        session.state.finish_expected_revision = None;
        session.state.finish_remove_stale = true;
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
        let payload = collection_commit(&session, edits);
        let reply = json!({"ok":true,"revision":new_revision,"generation":session.state.generation,"next_sequence":next_sequence});
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
            expected_revision,
            next_sequence,
            source_complete,
            remove_stale,
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
        if !snapshot_matches(&session, &sync_id, generation, source_revision)
            || next_sequence != session.state.next_sequence
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
                    json!({"ok":true,"revision":session.document_revision,"generation":generation,"complete":true,"cleanup_pending":false,"remove_stale":remove_stale}),
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
        }
        let mut stale: Vec<Marker> = if remove_stale {
            session
                .markers
                .values()
                .filter(|marker| marker.caller == session.caller && marker.generation < generation)
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        stale.sort_by(|left, right| left.source_id.cmp(&right.source_id));
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
        let mut edits = Vec::with_capacity(count * 2 + 1);
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
            let Some(revision) = session.document_revision.checked_add(1) else {
                return complete(
                    send,
                    context.request_id,
                    error("resource_exhausted", "catalog revision exhausted"),
                );
            };
            session.document_revision = revision;
        }
        session.state.cleanup_pending = !remaining.is_empty();
        edits.push(json!({
            "key":state_key(&session.caller),
            "value":session.state.clone(),
        }));
        let payload = collection_commit(&session, edits);
        let reply = json!({"ok":true,"revision":session.document_revision,"generation":session.state.generation,"complete":true,"cleanup_pending":!remaining.is_empty(),"remove_stale":session.state.finish_remove_stale});
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
                if payload["ok"] != true {
                    let failure = if payload["ok"] == false && payload["error"].is_object() {
                        payload
                    } else {
                        malformed()
                    };
                    if matches!(&pending.stage, Stage::ProviderSaving(_))
                        && let Some(caller) = &pending.context.importer
                    {
                        self.provider_sessions.remove(caller);
                    }
                    return complete(send, pending.context.request_id, failure);
                }
                let Some(revision) = payload["revision"].as_u64() else {
                    return complete(send, pending.context.request_id, malformed());
                };
                self.consume(pending.context, pending.stage, revision, payload, send)
            }
            failure => {
                if matches!(&pending.stage, Stage::ProviderSaving(_))
                    && let Some(caller) = &pending.context.importer
                {
                    self.provider_sessions.remove(caller);
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
            Stage::Saving(reply) => {
                self.provider_sessions
                    .retain(|_, session| session.key != context.key);
                complete(send, context.request_id, reply)
            }
            Stage::ProviderSaving(mut save) => {
                save.session.collection_revision = state_revision;
                if save.finish && !save.remaining_cleanup.is_empty() {
                    return self.commit_provider_cleanup(
                        context,
                        save.session,
                        save.remaining_cleanup,
                        send,
                    );
                }
                if let Some(caller) = &context.importer {
                    self.provider_sessions
                        .insert(caller.clone(), save.session.clone());
                }
                complete(send, context.request_id, save.reply)
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
                    entry_ids: BTreeSet::new(),
                    markers: BTreeMap::new(),
                    provider_state: None,
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
                        if scan
                            .markers
                            .values()
                            .any(|existing| existing.entry_id == marker.entry_id)
                        {
                            return complete(send, context.request_id, malformed());
                        }
                        scan.markers.insert(key.into(), marker);
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
                            .insert(key.into(), entry_fingerprint(&entry))
                            .is_some()
                        {
                            return complete(send, context.request_id, malformed());
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
                | Operation::ResolveExportedLaunch { id }
                | Operation::Update { id, .. }
                | Operation::Delete { id, .. } = &operation
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
                let public_count = count.saturating_sub(context.provider_count);
                if matches!(operation, Operation::Create { .. })
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
                Operation::ExportList { .. } | Operation::ResolveExportedLaunch { .. }
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
            Operation::ExportList { .. } | Operation::ResolveExportedLaunch { .. }
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
        let export = matches!(operation, Operation::ExportList { .. });
        let (query, cursor, limit) = match operation {
            Operation::List { cursor, limit } | Operation::ExportList { cursor, limit } => {
                (String::new(), cursor, limit)
            }
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
                (query.to_lowercase(), cursor, limit)
            }
            operation => {
                return match apply(operation, Some(document)) {
                    Ok(Change::Read(reply)) => complete_selected(send, context, reply),
                    Ok(Change::Write { document, reply }) => {
                        let id = reply["id"].as_str().expect("mutation ID");
                        let payload = json!({"action":"collection_commit","collection":context.key,"expected_revision":state_revision,"metadata":{"revision":document.revision,"next_id":document.next_id,"provider_count":context.provider_count},"edits":[{"key":id,"value":document.entries.get(id)}]});
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
            if cursor.query != query || !valid_collection_cursor(&cursor.after) {
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
            export,
            state_revision,
            revision: document.revision,
            query,
            limit,
            entries: Vec::new(),
            bytes: 0,
            scanned: 0,
            after: cursor.map(|cursor| cursor.after),
        };
        self.page(context, scan, send)
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
            if row["key"].as_str().is_some_and(is_provider_key) {
                scan.after = Some(row["key"].as_str().unwrap().into());
                scan.scanned += 1;
                continue;
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
            if matches_query(&entry, &scan.query) {
                let row = project_entry(id, &entry, scan.export);
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
                query: self.query,
            })
        } else {
            None
        };
        json!({"ok":true,"revision":self.revision,"entries":self.entries,"next_cursor":cursor})
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

fn header(metadata: &Value) -> Result<Document, Value> {
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

fn snapshot_matches(
    session: &ProviderSession,
    sync_id: &str,
    generation: u64,
    source_revision: u64,
) -> bool {
    session.state.generation == generation
        && session.state.sync_id == sync_id
        && session.state.source_revision == source_revision
}

fn revision_conflict(current_revision: u64) -> Value {
    json!({"ok":false,"error":{"code":"revision_conflict","message":"catalog revision differs","current_revision":current_revision}})
}

fn valid_collection_cursor(value: &str) -> bool {
    crate::model::parse_id(value).is_some() || is_provider_key(value)
}

fn is_provider_key(key: &str) -> bool {
    key.starts_with(ENTRY_PREFIX) || key.starts_with(STATE_PREFIX)
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
