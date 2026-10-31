//! Client-facing view of the configured catalog export.
//!
//! The catalog remains the source of truth and the launch service retains
//! ownership of launch operations and their retirement proofs.
mod artwork;
mod discovery;

use artwork::{Artwork, artwork_chunk, valid_artwork_cursor};

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub const SERVICE: &str = "amaterasu.library/1";
const CATALOG: &str = "amaterasu.catalog/1";
const MAX_PENDING: usize = 64;
const DEFAULT_LIST_LIMIT: usize = 20;
// Keep a minute of release fences without exhausting capacity during library browsing.
const MAX_TRANSFERS: usize = 4096;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    List {
        cursor: Option<Cursor>,
        limit: Option<usize>,
    },
    Resolve {
        id: String,
    },
    Discover {
        provider: String,
        cursor: Option<String>,
        limit: usize,
    },
    ArtworkRead {
        id: String,
        revision: Option<String>,
        offset: u64,
        transfer_id: String,
    },
    ArtworkRelease {
        id: String,
        transfer_id: String,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    revision: u64,
    after: String,
    query: String,
}

#[derive(Clone, Copy)]
enum CatalogProjection {
    Resolve,
    List { limit: usize },
}

impl Action {
    fn catalog_request(self) -> Result<(Value, CatalogProjection), &'static str> {
        match self {
            Self::List { cursor, limit } => {
                if limit.is_some_and(|n| !(1..=32).contains(&n))
                    || cursor
                        .as_ref()
                        .is_some_and(|c| !valid_id(&c.after) || !c.query.is_empty())
                {
                    return Err("invalid pagination");
                }
                let limit = limit.unwrap_or(DEFAULT_LIST_LIMIT);
                Ok((
                    json!({"action":"export_list","cursor":cursor,"limit":limit}),
                    CatalogProjection::List { limit },
                ))
            }
            Self::Resolve { id } if valid_id(&id) => Ok((
                json!({"action":"resolve_exported","id":id}),
                CatalogProjection::Resolve,
            )),
            Self::Resolve { .. } => Err("invalid catalog ID"),
            Self::ArtworkRead { .. } | Self::ArtworkRelease { .. } | Self::Discover { .. } => {
                Err("invalid request")
            }
        }
    }
}

fn valid_transfer_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn valid_id(id: &str) -> bool {
    id.len() == 20
        && id.starts_with("app-")
        && id[4..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && id[4..].bytes().any(|b| b != b'0')
}

fn error(code: &str, message: &str) -> Value {
    json!({"ok":false,"error":{"code":code,"message":message}})
}

/// Never forward launch details, paths, or unknown fields from the catalog.
fn project(payload: Value, projection: CatalogProjection) -> Value {
    if payload["ok"] == false {
        let code = payload
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("catalog_error");
        let message = payload
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("catalog request failed");
        if code.len() <= 128 && message.len() <= 512 {
            let mut result = error(code, message);
            if let Some(revision) = payload
                .pointer("/error/current_revision")
                .and_then(Value::as_u64)
            {
                result["error"]["current_revision"] = json!(revision);
            }
            return result;
        }
    } else if payload["ok"] == true
        && let Some(revision) = payload["revision"].as_u64()
    {
        if matches!(projection, CatalogProjection::Resolve) {
            if let (Some(id), Some(name)) = (payload["id"].as_str(), payload["name"].as_str())
                && valid_id(id)
                && valid_name(name)
            {
                return json!({"ok":true,"revision":revision,"id":id,"name":name});
            }
        } else if let CatalogProjection::List { limit } = projection
            && let Some(entries) = payload["entries"].as_array()
        {
            if entries.len() > limit {
                return error(
                    "invalid_catalog_response",
                    "catalog returned an invalid response",
                );
            }
            let projected: Option<Vec<Value>> = entries
                .iter()
                .map(|entry| {
                    let id = entry["id"].as_str()?;
                    let name = entry["name"].as_str()?;
                    (valid_id(id) && valid_name(name)).then(|| json!({"id":id,"name":name}))
                })
                .collect();
            let cursor = payload.get("next_cursor");
            if let (Some(entries), Some(cursor)) = (projected, cursor) {
                let valid_cursor = cursor.is_null()
                    || serde_json::from_value::<Cursor>(cursor.clone()).is_ok_and(|c| {
                        c.revision == revision && valid_id(&c.after) && c.query.is_empty()
                    });
                if valid_cursor {
                    return json!({"ok":true,"revision":revision,"entries":entries,"next_cursor":cursor});
                }
            }
        }
    }
    error(
        "invalid_catalog_response",
        "catalog returned an invalid response",
    )
}
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 512 && !name.chars().any(char::is_control)
}

enum Stage {
    Complete,
    Discovery {
        limit: usize,
    },
    Catalog {
        projection: CatalogProjection,
    },
    ArtworkResolve {
        revision: Option<String>,
        offset: u64,
        transfer_id: String,
    },
    ArtworkProvider {
        artwork: Artwork,
        revision: String,
        offset: u64,
        transfer_id: String,
    },
    ArtworkRelease {
        transfer_id: String,
    },
}
struct Transfer {
    initiator: String,
    id: String,
    artwork: Option<Artwork>,
    active: bool,
    release: Option<(u64, Instant)>,
    closed: bool,
    proven: bool,
    touched: Instant,
}
struct Pending {
    parent: u64,
    stage: Stage,
    result: Option<Value>,
    released: bool,
    deadline: Instant,
}
struct Admission {
    command: Command,
    pending: Pending,
}
struct Cleanup {
    command: Option<Command>,
    child: Option<u64>,
    result: Option<Value>,
    attempts: u64,
    failures: u32,
    retry_after: Instant,
}
impl Cleanup {
    fn defer_retry(&mut self) {
        self.failures = self.failures.saturating_add(1);
        let seconds = (1_u64 << self.failures.saturating_sub(1).min(5)).min(30);
        self.retry_after = Instant::now() + Duration::from_secs(seconds);
    }
}
struct Library {
    runtime: RuntimeClient,
    admissions: BTreeMap<u64, Admission>,
    pending: BTreeMap<u64, Pending>,
    discovery: discovery::Settings,
    transfers: BTreeMap<String, Transfer>,
    cleanup: BTreeMap<String, Cleanup>,
}
impl Library {
    fn retain_cleanup(&mut self, token: String) {
        if let Some(transfer) = self.transfers.get_mut(&token) {
            transfer.active = true;
            transfer.closed = true;
            transfer.release = None;
            self.cleanup.entry(token).or_insert(Cleanup {
                command: None,
                child: None,
                result: None,
                attempts: 0,
                failures: 0,
                retry_after: Instant::now(),
            });
        }
    }
    fn poll_cleanup(&mut self) {
        for (token, mut cleanup) in std::mem::take(&mut self.cleanup) {
            if Instant::now() < cleanup.retry_after {
                self.cleanup.insert(token, cleanup);
                continue;
            }
            if let Some(child) = cleanup.child {
                if cleanup.result.is_none() {
                    match self.runtime.send(Command::TakeResult { request: child }) {
                        Ok(Response::Result { payload }) => cleanup.result = Some(payload),
                        Ok(Response::Pending) | Err(_) => {
                            cleanup.retry_after = Instant::now() + Duration::from_millis(100);
                            self.cleanup.insert(token, cleanup);
                            continue;
                        }
                        _ => cleanup.result = Some(Value::Null),
                    }
                }
                let released = match self.runtime.send(Command::ReleaseResult { request: child }) {
                    Ok(Response::Ok) => true,
                    Ok(Response::Error { code, .. }) if code == "not_found" => true,
                    _ => false,
                };
                if !released {
                    cleanup.retry_after = Instant::now() + Duration::from_millis(100);
                    self.cleanup.insert(token, cleanup);
                    continue;
                }
                let result = cleanup.result.take().unwrap();
                if result["ok"] == true && result["transfer_id"].as_str() == Some(&token) {
                    if let Some(transfer) = self.transfers.get_mut(&token) {
                        transfer.active = false;
                        transfer.closed = true;
                        transfer.proven = true;
                        transfer.touched = Instant::now();
                    }
                    continue;
                }
                cleanup.child = None;
                cleanup.command = None;
                cleanup.defer_retry();
            }
            if cleanup.child.is_none() && Instant::now() >= cleanup.retry_after {
                if cleanup.command.is_none() {
                    let Some(next_attempt) = cleanup.attempts.checked_add(1) else {
                        self.cleanup.insert(token, cleanup);
                        continue;
                    };
                    cleanup.attempts = next_attempt;
                    cleanup.command = Some(Command::Request {
                        service: "amaterasu.playnite/1".into(),
                        payload: json!({"action":"artwork_release","transfer_id":token}),
                        timeout_ms: 10_000,
                        operation_id: format!("library.cleanup.{token}.{}", cleanup.attempts),
                        parent: None,
                    });
                }
                match self.runtime.send(cleanup.command.as_ref().unwrap().clone()) {
                    Ok(Response::Accepted { id }) => cleanup.child = Some(id),
                    Err(_) => {
                        cleanup.defer_retry();
                    } // Retry identical admission after a lost reply.
                    _ => {
                        cleanup.command = None;
                        cleanup.defer_retry();
                    }
                }
            }
            // Keep retrying at a bounded rate until a matching acknowledgment proves release.
            self.cleanup.insert(token, cleanup);
        }
    }
    fn queue_completion(&mut self, parent: u64, payload: Value, deadline: Instant) {
        self.admissions.insert(
            parent,
            Admission {
                command: Command::Complete {
                    request: parent,
                    payload,
                },
                pending: Pending {
                    parent,
                    stage: Stage::Complete,
                    result: None,
                    released: true,
                    deadline,
                },
            },
        );
    }
}
impl ManagedModule for Library {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("library settings unavailable".into());
        };
        let discovery = discovery::Settings::from_module_settings(&payload)?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            admissions: BTreeMap::new(),
            pending: BTreeMap::new(),
            discovery,
            transfers: BTreeMap::new(),
            cleanup: BTreeMap::new(),
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        self.poll_cleanup();
        self.transfers.retain(|_, transfer| {
            transfer.active
                || transfer.release.is_some()
                || transfer.touched.elapsed() < Duration::from_secs(60)
        });
        let ready: Vec<_> = self
            .transfers
            .iter()
            .filter_map(|(token, t)| {
                (!t.active)
                    .then_some(t.release)
                    .flatten()
                    .map(|(parent, deadline)| (token.clone(), parent, deadline))
            })
            .collect();
        for (token, parent, deadline) in ready {
            let transfer = self.transfers.get_mut(&token).unwrap();
            transfer.release = None;
            transfer.closed = true;
            transfer.touched = Instant::now();
            if matches!(transfer.artwork, Some(Artwork::Playnite { .. })) {
                let timeout = deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(30_000) as u64;
                if timeout > 0 {
                    transfer.active = true;
                    self.admissions.insert(
                        parent,
                        Admission {
                            command: Command::Request {
                                service: "amaterasu.playnite/1".into(),
                                payload: json!({"action":"artwork_release","transfer_id":token}),
                                timeout_ms: timeout,
                                operation_id: format!("library.artwork_release.{parent}"),
                                parent: Some(parent),
                            },
                            pending: Pending {
                                parent,
                                stage: Stage::ArtworkRelease { transfer_id: token },
                                result: None,
                                released: false,
                                deadline,
                            },
                        },
                    );
                    continue;
                }
                self.retain_cleanup(token);
                self.queue_completion(
                    parent,
                    error("service_unavailable", "artwork release timed out"),
                    deadline,
                );
            } else {
                transfer.proven = true;
                self.queue_completion(parent, json!({"ok":true}), deadline);
            }
        }
        for (parent, admission) in std::mem::take(&mut self.admissions) {
            let completing = matches!(admission.command, Command::Complete { .. });
            match self.runtime.send(admission.command.clone()) {
                Ok(Response::Ok) | Ok(Response::Error { .. }) if completing => {}
                Ok(Response::Accepted { id }) => {
                    self.pending.insert(id, admission.pending);
                }
                Err(_) => {
                    self.admissions.insert(parent, admission);
                }
                _ => {
                    let mut pending = admission.pending;
                    if let Stage::ArtworkResolve {
                        ref transfer_id, ..
                    }
                    | Stage::ArtworkProvider {
                        ref transfer_id, ..
                    } = pending.stage
                        && let Some(t) = self.transfers.get_mut(transfer_id)
                    {
                        t.active = false;
                        t.touched = Instant::now();
                    }
                    if let Stage::ArtworkRelease { ref transfer_id } = pending.stage
                        && let Some(t) = self.transfers.get_mut(transfer_id)
                    {
                        t.active = false;
                        t.closed = false;
                        t.touched = Instant::now();
                    }
                    if let Stage::ArtworkRelease { ref transfer_id } = pending.stage {
                        self.retain_cleanup(transfer_id.clone());
                    }
                    let code = match pending.stage {
                        Stage::Discovery { .. } => "provider_unavailable",
                        Stage::Catalog { .. } => "catalog_unavailable",
                        _ => "service_unavailable",
                    };
                    pending.stage = Stage::Complete;
                    pending.result = Some(error(code, "child request could not be admitted"));
                    pending.released = true;
                    // This key is synthetic and cannot collide with a tracked child: use the
                    // parent map for completions instead of storing it as a child.
                    if !matches!(
                        self.runtime.send(Command::Complete {
                            request: parent,
                            payload: pending.result.as_ref().unwrap().clone()
                        }),
                        Ok(Response::Ok) | Ok(Response::Error { .. })
                    ) {
                        self.admissions.insert(
                            parent,
                            Admission {
                                command: Command::Complete {
                                    request: parent,
                                    payload: pending.result.unwrap(),
                                },
                                pending: Pending {
                                    parent,
                                    stage: Stage::Complete,
                                    result: None,
                                    released: true,
                                    deadline: pending.deadline,
                                },
                            },
                        );
                    }
                }
            }
        }
        let pending_requests = std::mem::take(&mut self.pending);
        for (child, mut pending) in pending_requests {
            if pending.result.is_none() {
                match self.runtime.send(Command::TakeResult { request: child }) {
                    Ok(Response::Result { payload }) => pending.result = Some(payload),
                    Ok(Response::Pending) | Err(_) => {
                        self.pending.insert(child, pending);
                        continue;
                    }
                    _ => {
                        let code = match pending.stage {
                            Stage::Discovery { .. } => "provider_unavailable",
                            Stage::Catalog { .. } => "catalog_unavailable",
                            _ => "service_unavailable",
                        };
                        pending.result = Some(error(code, "child request failed"))
                    }
                }
            }
            if !pending.released {
                pending.released =
                    match self.runtime.send(Command::ReleaseResult { request: child }) {
                        Ok(Response::Ok) => true,
                        Ok(Response::Error { code, .. }) if code == "not_found" => true,
                        _ => false,
                    };
            }
            if !pending.released {
                self.pending.insert(child, pending);
                continue;
            }
            let payload = pending.result.take().unwrap();
            match pending.stage {
                Stage::ArtworkResolve {
                    revision,
                    offset,
                    transfer_id,
                } if payload["ok"] == true => {
                    let artwork =
                        serde_json::from_value::<Artwork>(payload["artwork"].clone()).ok();
                    let transfer = self.transfers.get_mut(&transfer_id).unwrap();
                    let provider = artwork
                        .as_ref()
                        .filter(|a| {
                            transfer
                                .artwork
                                .as_ref()
                                .is_none_or(|existing| existing == *a)
                        })
                        .and_then(|a| a.request(revision.as_deref(), offset, &transfer_id));
                    if let (Some(artwork), Some((service, request))) = (artwork, provider) {
                        transfer.artwork = Some(artwork.clone());
                        let revision = artwork.revision().to_owned();
                        let timeout = pending
                            .deadline
                            .saturating_duration_since(Instant::now())
                            .as_millis()
                            .min(30_000) as u64;
                        if timeout > 0 {
                            self.admissions.insert(
                                pending.parent,
                                Admission {
                                    command: Command::Request {
                                        service: service.into(),
                                        payload: request,
                                        timeout_ms: timeout,
                                        operation_id: format!("library.artwork.{}", pending.parent),
                                        parent: Some(pending.parent),
                                    },
                                    pending: Pending {
                                        parent: pending.parent,
                                        stage: Stage::ArtworkProvider {
                                            artwork,
                                            revision,
                                            offset,
                                            transfer_id,
                                        },
                                        result: None,
                                        released: false,
                                        deadline: pending.deadline,
                                    },
                                },
                            );
                            continue;
                        }
                    }
                    if let Some(t) = self.transfers.get_mut(&transfer_id) {
                        t.active = false;
                        t.touched = Instant::now();
                    }
                    pending.result = Some(error(
                        "artwork_unavailable",
                        "artwork unavailable or changed",
                    ));
                }
                Stage::ArtworkResolve { transfer_id, .. } => {
                    if let Some(t) = self.transfers.get_mut(&transfer_id) {
                        t.active = false;
                        t.touched = Instant::now();
                    }
                    pending.result = Some(error("artwork_unavailable", "artwork unavailable"))
                }
                Stage::ArtworkProvider {
                    artwork,
                    revision,
                    offset,
                    transfer_id,
                } => {
                    if let Some(t) = self.transfers.get_mut(&transfer_id) {
                        t.active = false;
                        t.touched = Instant::now();
                    }
                    pending.result = Some(
                        if matches!(artwork, Artwork::Playnite { .. })
                            && payload["transfer_id"].as_str() != Some(&transfer_id)
                        {
                            error(
                                "invalid_artwork_response",
                                "provider returned invalid transfer",
                            )
                        } else {
                            artwork_chunk(payload, &artwork, &revision, offset)
                        },
                    );
                }
                Stage::ArtworkRelease { transfer_id } => {
                    if let Some(t) = self.transfers.get_mut(&transfer_id) {
                        t.active = false;
                        t.closed = payload["ok"] == true
                            && payload["transfer_id"].as_str() == Some(&transfer_id);
                        t.proven = t.closed;
                        t.touched = Instant::now();
                    }
                    if payload["ok"] != true
                        || payload["transfer_id"].as_str() != Some(&transfer_id)
                    {
                        self.retain_cleanup(transfer_id.clone());
                    }
                    pending.result = Some(
                        if payload["ok"] == true
                            && payload["transfer_id"].as_str() == Some(&transfer_id)
                        {
                            json!({"ok":true})
                        } else {
                            error("service_unavailable", "artwork release failed")
                        },
                    );
                }
                Stage::Catalog { projection } => {
                    pending.result = Some(project(payload, projection))
                }
                Stage::Discovery { limit } => {
                    pending.result = Some(discovery::project(payload, limit))
                }
                Stage::Complete => pending.result = Some(payload),
            }
            if !matches!(
                self.runtime.send(Command::Complete {
                    request: pending.parent,
                    payload: pending.result.as_ref().unwrap().clone()
                }),
                Ok(Response::Ok) | Ok(Response::Error { .. })
            ) {
                // Keep the already-released child until the parent completion is acknowledged.
                pending.stage = Stage::Complete;
                self.pending.insert(child, pending);
            }
        }
        let request = match self.runtime.send_ephemeral(Command::Poll {})? {
            Response::Request {
                id,
                caller,
                initiator,
                payload,
                remaining_ms,
                ..
            } => (id, caller, initiator, payload, remaining_ms),
            Response::Pending => return Ok(()),
            _ => return Err("unexpected poll response".into()),
        };
        let (id, caller, initiator, payload, remaining_ms) = request;
        let invalid = if caller != initiator {
            Some(error("forbidden", "direct caller required"))
        } else if self.pending.len() + self.admissions.len() >= MAX_PENDING {
            Some(error("busy", "library is busy"))
        } else {
            None
        };
        if let Some(payload) = invalid {
            let _ = self.runtime.send_ephemeral(Command::Complete {
                request: id,
                payload,
            });
            return Ok(());
        }
        let action = serde_json::from_value::<Action>(payload);
        let prepared = match action {
            Ok(Action::ArtworkRead {
                id: app_id,
                revision,
                offset,
                transfer_id,
            }) if valid_id(&app_id)
                && valid_transfer_id(&transfer_id)
                && valid_artwork_cursor(revision.as_deref(), offset) =>
            {
                if !self.transfers.contains_key(&transfer_id)
                    && (self.transfers.len() >= MAX_TRANSFERS || offset != 0)
                {
                    let _ = self.runtime.send(Command::Complete {
                        request: id,
                        payload: error("invalid_transfer", "artwork transfer unavailable"),
                    });
                    return Ok(());
                }
                let transfer = self
                    .transfers
                    .entry(transfer_id.clone())
                    .or_insert_with(|| Transfer {
                        initiator: initiator.clone(),
                        id: app_id.clone(),
                        artwork: None,
                        active: false,
                        release: None,
                        closed: false,
                        proven: false,
                        touched: Instant::now(),
                    });
                if transfer.initiator != initiator
                    || transfer.id != app_id
                    || transfer.closed
                    || transfer.release.is_some()
                    || transfer.active
                {
                    Err(("invalid_transfer", "artwork transfer unavailable"))
                } else if transfer.artwork.is_none() && offset != 0 {
                    Err((
                        "invalid_transfer",
                        "artwork transfer must begin at offset zero",
                    ))
                } else {
                    transfer.active = true;
                    transfer.touched = Instant::now();
                    Ok((
                        CATALOG.to_owned(),
                        json!({"action":"resolve_exported_artwork","id":app_id}),
                        Stage::ArtworkResolve {
                            revision,
                            offset,
                            transfer_id,
                        },
                    ))
                }
            }
            Ok(Action::ArtworkRelease {
                id: app_id,
                transfer_id,
            }) if valid_id(&app_id) && valid_transfer_id(&transfer_id) => {
                if !self.transfers.contains_key(&transfer_id)
                    && self.transfers.len() < MAX_TRANSFERS
                {
                    self.transfers.insert(
                        transfer_id.clone(),
                        Transfer {
                            initiator: initiator.clone(),
                            id: app_id.clone(),
                            artwork: None,
                            active: false,
                            release: None,
                            closed: true,
                            proven: false,
                            touched: Instant::now(),
                        },
                    );
                }
                if let Some(transfer) = self.transfers.get_mut(&transfer_id) {
                    if transfer.initiator != initiator
                        || transfer.id != app_id
                        || transfer.release.is_some()
                        || (transfer.closed && transfer.active)
                    {
                        Err(("invalid_transfer", "artwork transfer unavailable"))
                    } else if transfer.closed {
                        let result = if transfer.proven {
                            json!({"ok":true})
                        } else {
                            error("service_unavailable", "artwork release is unverified")
                        };
                        self.queue_completion(
                            id,
                            result,
                            Instant::now() + Duration::from_millis(remaining_ms.min(30_000)),
                        );
                        return Ok(());
                    } else {
                        transfer.release = Some((
                            id,
                            Instant::now() + Duration::from_millis(remaining_ms.min(30_000)),
                        ));
                        return Ok(());
                    }
                } else {
                    Err(("busy", "artwork transfer capacity reached"))
                }
            }
            Ok(Action::Discover {
                provider,
                cursor,
                limit,
            }) => self
                .discovery
                .route(discovery::Request::new(provider, cursor, limit), &initiator)
                .map(|(service, payload, limit)| (service, payload, Stage::Discovery { limit }))
                .map_err(|code| (code, "discovery request rejected")),
            Ok(action) => action
                .catalog_request()
                .map(|(payload, projection)| {
                    (CATALOG.to_owned(), payload, Stage::Catalog { projection })
                })
                .map_err(|message| ("invalid_request", message)),
            Err(_) => Err(("invalid_request", "invalid request")),
        };
        let (service, payload, stage) = match prepared {
            Ok(value) => value,
            Err((code, message)) => {
                let _ = self.runtime.send_ephemeral(Command::Complete {
                    request: id,
                    payload: error(code, message),
                });
                return Ok(());
            }
        };
        self.admissions.insert(
            id,
            Admission {
                command: Command::Request {
                    service,
                    payload,
                    timeout_ms: remaining_ms.min(30_000),
                    operation_id: format!("library.catalog.{id}"),
                    parent: Some(id),
                },
                pending: Pending {
                    parent: id,
                    stage,
                    result: None,
                    released: false,
                    deadline: Instant::now() + Duration::from_millis(remaining_ms.min(30_000)),
                },
            },
        );
        Ok(())
    }
}
amaterasu_module_sdk::export_module!(Library);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn artwork_reference_routes_only_known_providers_and_exact_revisions() {
        let artwork: Artwork =
            serde_json::from_value(json!({"provider":"steam","app_id":42,"revision":"17"}))
                .unwrap();
        assert_eq!(
            artwork
                .request(Some("17"), 0, "00000000000000000000000000000000")
                .unwrap()
                .0,
            "amaterasu.steam/1"
        );
        assert!(
            artwork
                .request(Some("18"), 0, "00000000000000000000000000000000")
                .is_none()
        );
        assert!(
            artwork
                .request(None, 0, "00000000000000000000000000000000")
                .is_some()
        );
        assert!(valid_artwork_cursor(None, 0));
        assert!(!valid_artwork_cursor(None, 8192));
        assert!(valid_artwork_cursor(Some("17"), 8192));
        assert!(!valid_artwork_cursor(Some("17"), 1));
        assert!(!valid_artwork_cursor(Some("17"), 16 * 1024 * 1024));
        assert!(
            serde_json::from_value::<Artwork>(
                json!({"provider":"steam","app_id":42,"revision":"17","path":"/secret"})
            )
            .is_err()
        );
        assert!(serde_json::from_value::<Action>(json!({"action":"artwork_read","id":"app-0000000000000001","revision":"17","offset":0,"provider":"steam"})).is_err());
    }
    #[test]
    fn artwork_chunk_rejects_identity_revision_cursor_and_size_changes() {
        let artwork = Artwork::Steam {
            app_id: 42,
            revision: "17".into(),
        };
        let chunk = json!({"ok":true,"app_id":42,"revision":"17","offset":0,"next_offset":null,"total_bytes":3,"mime_type":"image/png","data":"YWJj","path":"secret"});
        let clean = artwork_chunk(chunk.clone(), &artwork, "17", 0);
        assert_eq!(clean["data"], "YWJj");
        assert!(clean.get("path").is_none());
        for (key, value) in [
            ("app_id", json!(43)),
            ("revision", json!("18")),
            ("offset", json!(1)),
            ("total_bytes", json!(16777217)),
            ("next_offset", json!(3)),
            ("data", json!("!!!!")),
            ("mime_type", json!("text/html")),
        ] {
            let mut invalid = chunk.clone();
            invalid[key] = value;
            assert_eq!(
                artwork_chunk(invalid, &artwork, "17", 0)["ok"],
                false,
                "{key}"
            );
        }
    }
    #[test]
    fn resolve_never_exposes_launch_or_configuration() {
        let result = project(
            json!({"ok":true,"revision":7,"id":"app-0000000000000001","name":"Game","entry":{"launch":{"program":"secret"},"prep_cmd":["secret"]}}),
            CatalogProjection::Resolve,
        );
        assert_eq!(
            result,
            json!({"ok":true,"revision":7,"id":"app-0000000000000001","name":"Game"})
        );
    }
    #[test]
    fn validates_requests_and_sanitizes_pages() {
        assert!(
            serde_json::from_value::<Action>(
                json!({"action":"resolve","id":"app-0000000000000001","owner":"other"})
            )
            .is_err()
        );
        assert!(
            Action::List {
                cursor: None,
                limit: Some(33)
            }
            .catalog_request()
            .is_err()
        );
        let result = project(
            json!({"ok":true,"revision":1,"entries":[{"id":"app-0000000000000001","name":"Game","launch":"secret"}],"next_cursor":null}),
            CatalogProjection::List { limit: 20 },
        );
        assert_eq!(
            result["entries"][0],
            json!({"id":"app-0000000000000001","name":"Game"})
        );
    }
}

#[cfg(test)]
mod artwork_tests;

#[cfg(test)]
mod forwarding_tests {
    use super::*;
    use amaterasu_module_sdk::abi::{
        AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_INVALID_ARGUMENT, STATUS_OK, Status,
    };
    use std::ffi::c_void;

    #[derive(Default)]
    struct Host {
        polled: bool,
        commands: Vec<Command>,
        completed: Option<Value>,
        requested: Option<Value>,
        catalog_result: Option<Value>,
    }
    unsafe extern "C" fn invoke(
        context: *mut c_void,
        input: BorrowedBytes,
        reply: ReplySink,
    ) -> Status {
        // SAFETY: the test retains the boxed host and input for this synchronous call.
        let Ok(bytes) = (unsafe { input.read() }) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
            return STATUS_INVALID_ARGUMENT;
        };
        // SAFETY: context points to the live boxed test host.
        let host = unsafe { &mut *(context as *mut Host) };
        let response = match &command {
            Command::GetSettings {} => Response::Result { payload: json!({}) },
            Command::Register { .. } | Command::ReleaseResult { .. } => Response::Ok,
            Command::Poll {} if !host.polled => {
                host.polled = true;
                Response::Request {
                    id: 11,
                    caller: "amaterasu.nvhttp".into(),
                    initiator: "amaterasu.nvhttp".into(),
                    service: SERVICE.into(),
                    payload: host.requested.clone().unwrap_or_else(|| json!({"action":"list","limit":20})),
                    remaining_ms: 5000,
                }
            }
            Command::Poll {} => Response::Pending,
            Command::Request { .. } => Response::Accepted { id: 12 },
            Command::TakeResult { request: 12 } => Response::Result {
                payload: host.catalog_result.clone().unwrap_or_else(|| json!({"ok":true,"revision":4,"entries":[{"id":"app-0000000000000001","name":"Game","launch":"private"}],"next_cursor":null})),
            },
            Command::Complete {
                request: 11,
                payload,
            } => {
                host.completed = Some(payload.clone());
                Response::Ok
            }
            _ => return STATUS_INVALID_ARGUMENT,
        };
        host.commands.push(command);
        let encoded = serde_json::to_vec(&response).unwrap();
        // SAFETY: the reply receiver copies encoded before this call returns.
        unsafe { reply.send(&encoded) };
        STATUS_OK
    }
    #[test]
    fn forwards_export_as_child_and_completes_with_sanitized_metadata() {
        let mut host = Box::<Host>::default();
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&mut *host as *mut Host).cast(),
            invoke,
        };
        // SAFETY: endpoint and host remain live for the lifetime of the test module.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut module = Library::start(runtime).unwrap();
        module.tick().unwrap();
        module.tick().unwrap();
        module.tick().unwrap();
        assert!(host.commands.iter().any(|command| matches!(command, Command::Request { service, payload, parent: Some(11), .. } if service == CATALOG && payload["action"] == "export_list")));
        assert_eq!(
            host.completed,
            Some(
                json!({"ok":true,"revision":4,"entries":[{"id":"app-0000000000000001","name":"Game"}],"next_cursor":null})
            )
        );
    }
    #[test]
    fn list_response_respects_requested_and_default_page_limit_through_broker() {
        for (requested, limit, count, accepted) in [
            (Some(1), 1, 1, true),
            (Some(1), 1, 2, false),
            (Some(32), 32, 32, true),
            (Some(32), 32, 33, false),
            (None, 20, 20, true),
            (None, 20, 21, false),
        ] {
            let entries: Vec<_> = (1..=count)
                .map(|n| json!({"id":format!("app-{n:016x}"),"name":"Game"}))
                .collect();
            let mut host = Box::new(Host {
                requested: Some(json!({"action":"list","limit":requested})),
                catalog_result: Some(
                    json!({"ok":true,"revision":4,"entries":entries,"next_cursor":null}),
                ),
                ..Default::default()
            });
            let endpoint = HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: (&mut *host as *mut Host).cast(),
                invoke,
            };
            // SAFETY: host and endpoint remain live for the module's synchronous calls.
            let mut module =
                Library::start(unsafe { RuntimeClient::from_endpoint(endpoint) }).unwrap();
            for _ in 0..3 {
                module.tick().unwrap();
            }
            assert!(host.commands.iter().any(|command| matches!(command, Command::Request { service, payload, parent: Some(11), .. } if service == CATALOG && payload["limit"] == limit)));
            let response = host.completed.as_ref().unwrap();
            assert_eq!(response["ok"], accepted, "limit {limit}, count {count}");
            if !accepted {
                assert_eq!(response["error"]["code"], "invalid_catalog_response");
            }
        }
    }
}
