//! Client-facing view of the configured catalog export.
//!
//! The catalog remains the source of truth and the launch service retains
//! ownership of launch operations and their retirement proofs.
mod artwork;
mod discovery;

use amaterasu_module_sdk::application_library::artwork::{ArtworkRoutes, supports_artwork};
use amaterasu_module_sdk::artwork_transfer::{self, Offer, Source};
use artwork::Artwork;

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
    ArtworkOpen {
        id: String,
        revision: Option<String>,
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
            Self::ArtworkOpen { .. } | Self::Discover { .. } => Err("invalid request"),
        }
    }
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
    Catalog(CatalogProjection),
    Discovery(usize),
    ResolveArtwork(Option<String>),
    DescribeArtwork {
        artwork: Artwork,
        service: String,
        request: Value,
    },
    OpenArtwork {
        artwork: Artwork,
        service: String,
    },
    Complete,
}
struct Pending {
    parent: u64,
    owner: String,
    stage: Stage,
    result: Option<Value>,
    released: bool,
    deadline: Instant,
}
struct Admission {
    command: Command,
    pending: Pending,
}
struct Library {
    runtime: RuntimeClient,
    admissions: BTreeMap<u64, Admission>,
    pending: BTreeMap<u64, Pending>,
    discovery: discovery::Settings,
    artwork_routes: ArtworkRoutes,
    artwork: Source,
}
impl Library {
    fn enqueue(&mut self, pending: Pending, service: String, payload: Value) {
        let timeout_ms = pending
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(30_000) as u64;
        let operation_id = format!(
            "library.{}.{}",
            pending.parent,
            match &pending.stage {
                Stage::ResolveArtwork(_) => "resolve",
                Stage::DescribeArtwork { .. } => "describe",
                Stage::OpenArtwork { .. } => "open",
                _ => "request",
            }
        );
        self.admissions.insert(
            pending.parent,
            Admission {
                command: Command::Request {
                    service,
                    payload,
                    timeout_ms,
                    operation_id,
                    parent: Some(pending.parent),
                },
                pending,
            },
        );
    }
    fn complete(&mut self, mut pending: Pending, payload: Value) {
        let command = Command::Complete {
            request: pending.parent,
            payload,
        };
        if matches!(
            self.runtime.send(command.clone()),
            Ok(Response::Ok) | Ok(Response::Error { .. })
        ) {
            return;
        }
        pending.stage = Stage::Complete;
        self.admissions
            .insert(pending.parent, Admission { command, pending });
    }
    fn advance(&mut self, mut pending: Pending, payload: Value) {
        let stage = std::mem::replace(&mut pending.stage, Stage::Complete);
        let response = match stage {
            Stage::Catalog(projection) => project(payload, projection),
            Stage::Discovery(limit) => discovery::project(payload, limit),
            Stage::ResolveArtwork(expected) => {
                if payload["ok"] == true
                    && let Ok(artwork) =
                        serde_json::from_value::<Artwork>(payload["artwork"].clone())
                    && expected
                        .as_deref()
                        .is_none_or(|expected| expected == artwork.revision())
                    && let Some((service, request)) =
                        artwork.route(&self.artwork_routes, &pending.owner)
                {
                    if matches!(artwork, Artwork::External { .. }) {
                        pending.stage = Stage::DescribeArtwork {
                            artwork,
                            service: service.clone(),
                            request,
                        };
                        self.enqueue(pending, service, json!({"action":"describe"}));
                    } else {
                        pending.stage = Stage::OpenArtwork {
                            artwork,
                            service: service.clone(),
                        };
                        self.enqueue(pending, service, request);
                    }
                    return;
                }
                error("artwork_unavailable", "artwork unavailable")
            }
            Stage::DescribeArtwork {
                artwork,
                service,
                request,
            } => {
                if supports_artwork(payload) {
                    pending.stage = Stage::OpenArtwork {
                        artwork,
                        service: service.clone(),
                    };
                    self.enqueue(pending, service, request);
                    return;
                }
                error(
                    "artwork_unsupported",
                    "selected provider does not support artwork",
                )
            }
            Stage::OpenArtwork { artwork, service } => {
                let result = (|| -> ModuleResult<Offer> {
                    if payload["ok"] != true {
                        return Err("artwork unavailable".into());
                    }
                    let offer: Offer = serde_json::from_value(payload["offer"].clone())
                        .map_err(|e| e.to_string())?;
                    if offer.revision != artwork.revision() {
                        return Err("artwork revision changed".into());
                    }
                    // SAFETY: service comes only from catalog validation and startup routing;
                    // native grants and dependency ordering retain its registered factory.
                    let image =
                        unsafe { artwork_transfer::receive(&self.runtime, &service, offer)? };
                    self.artwork
                        .publish(&pending.owner, image.revision, image.mime_type, image.bytes)
                        .map_err(str::to_owned)
                })();
                match result {
                    Ok(offer) => json!({"ok":true,"offer":offer}),
                    Err(_) => error("artwork_unavailable", "artwork transfer failed"),
                }
            }
            Stage::Complete => payload,
        };
        self.complete(pending, response);
    }
}
impl ManagedModule for Library {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("library settings unavailable".into());
        };
        let discovery = discovery::Settings::from_module_settings(&payload)?;
        let artwork_routes = ArtworkRoutes::from_module_settings(&payload)?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        let artwork = Source::new()?;
        // SAFETY: owned factory lives for this module; consumers depend on it at startup.
        unsafe {
            artwork.register(&runtime, SERVICE)?;
        }
        Ok(Self {
            runtime,
            admissions: BTreeMap::new(),
            pending: BTreeMap::new(),
            discovery,
            artwork_routes,
            artwork,
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        self.artwork.reap();
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
                _ => self.complete(
                    admission.pending,
                    error("service_unavailable", "child request rejected"),
                ),
            }
        }
        for (child, mut pending) in std::mem::take(&mut self.pending) {
            if pending.result.is_none() {
                match self.runtime.send(Command::TakeResult { request: child }) {
                    Ok(Response::Result { payload }) => pending.result = Some(payload),
                    Ok(Response::Pending) | Err(_) => {
                        self.pending.insert(child, pending);
                        continue;
                    }
                    _ => {
                        pending.result = Some(error("service_unavailable", "child request failed"))
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
            self.advance(pending, payload);
        }
        let Response::Request {
            id,
            caller,
            initiator,
            payload,
            remaining_ms,
            ..
        } = self.runtime.send_ephemeral(Command::Poll {})?
        else {
            return Ok(());
        };
        let pending = Pending {
            parent: id,
            owner: caller.clone(),
            stage: Stage::Complete,
            result: None,
            released: false,
            deadline: Instant::now() + Duration::from_millis(remaining_ms.min(30_000)),
        };
        if caller != initiator {
            self.complete(pending, error("forbidden", "direct caller required"));
            return Ok(());
        }
        if self.pending.len() + self.admissions.len() >= MAX_PENDING {
            self.complete(pending, error("busy", "library is busy"));
            return Ok(());
        }
        let action = serde_json::from_value::<Action>(payload);
        let prepared = match action {
            Ok(Action::ArtworkOpen { id, revision })
                if valid_id(&id)
                    && revision.as_ref().is_none_or(|r| {
                        !r.is_empty() && r.len() <= 128 && !r.chars().any(char::is_control)
                    }) =>
            {
                Ok((
                    CATALOG.into(),
                    json!({"action":"resolve_exported_artwork","id":id}),
                    Stage::ResolveArtwork(revision),
                ))
            }
            Ok(Action::Discover {
                provider,
                cursor,
                limit,
            }) => self
                .discovery
                .route(discovery::Request::new(provider, cursor, limit), &initiator)
                .map(|(service, payload, limit)| (service, payload, Stage::Discovery(limit)))
                .map_err(|_| "discovery rejected"),
            Ok(action) => action
                .catalog_request()
                .map(|(payload, projection)| (CATALOG.into(), payload, Stage::Catalog(projection))),
            Err(_) => Err("invalid request"),
        };
        match prepared {
            Ok((service, payload, stage)) => {
                self.enqueue(Pending { stage, ..pending }, service, payload)
            }
            Err(message) => self.complete(pending, error("invalid_request", message)),
        }
        Ok(())
    }
}
amaterasu_module_sdk::export_module!(Library);

#[cfg(test)]
mod artwork_tests;

#[cfg(test)]
mod projection_tests {
    use super::*;
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
            Command::Register { .. } | Command::ReleaseResult { .. } | Command::NativeRegisterTyped { .. } => Response::Ok,
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
