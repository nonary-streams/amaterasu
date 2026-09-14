use crate::STATE_SERVICE;
use crate::model::{Document, Entry, MAX_ENTRIES, bounded_text};
use crate::protocol::{Change, Cursor, Operation, apply, error, matches_query, project_entry};
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
    key: String,
    catalog_owner: Option<String>,
    operation: Operation,
    deadline: Instant,
}
struct Context {
    request_id: u64,
    key: String,
    catalog_owner: Option<String>,
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
    queued: VecDeque<Incoming>,
    pending: Option<Pending>,
    sequence: u64,
}
impl Pipeline {
    pub fn new(
        launch_principals: BTreeSet<String>,
        export_principals: BTreeMap<String, String>,
    ) -> Self {
        Self {
            launch_principals,
            export_principals,
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
                deadline: incoming.deadline,
            };
            self.load(context, incoming.operation, &mut send)?;
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
                    return complete(send, pending.context.request_id, failure);
                }
                let Some(revision) = payload["revision"].as_u64() else {
                    return complete(send, pending.context.request_id, malformed());
                };
                self.consume(pending.context, pending.stage, revision, payload, send)
            }
            failure => complete(send, pending.context.request_id, runtime_failure(failure)),
        }
    }
    fn consume(
        &mut self,
        context: Context,
        stage: Stage,
        state_revision: u64,
        payload: Value,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        match stage {
            Stage::Saving(reply) => complete(send, context.request_id, reply),
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
                if matches!(operation, Operation::Create { .. }) && count >= MAX_ENTRIES as u64 {
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
        let metadata = json!({"revision":document.revision,"next_id":document.next_id});
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
                        let payload = json!({"action":"collection_commit","collection":context.key,"expected_revision":state_revision,"metadata":{"revision":document.revision,"next_id":document.next_id},"edits":[{"key":id,"value":document.entries.get(id)}]});
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
            if cursor.query != query || crate::model::parse_id(&cursor.after).is_none() {
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
    value["entries"] = json!({});
    let document: Document = serde_json::from_value(value).map_err(|_| malformed())?;
    document.validate().map_err(|_| malformed())?;
    Ok(document)
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
