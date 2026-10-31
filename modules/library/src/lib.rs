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
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    revision: u64,
    after: String,
    query: String,
}

impl Action {
    fn catalog_request(self) -> Result<(Value, bool), &'static str> {
        match self {
            Self::List { cursor, limit } => {
                if limit.is_some_and(|n| !(1..=32).contains(&n))
                    || cursor
                        .as_ref()
                        .is_some_and(|c| !valid_id(&c.after) || !c.query.is_empty())
                {
                    return Err("invalid pagination");
                }
                Ok((
                    json!({"action":"export_list","cursor":cursor,"limit":limit}),
                    false,
                ))
            }
            Self::Resolve { id } if valid_id(&id) => {
                Ok((json!({"action":"resolve_exported","id":id}), true))
            }
            Self::Resolve { .. } => Err("invalid catalog ID"),
            Self::ArtworkRead { .. } | Self::Discover { .. } => Err("invalid request"),
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
fn project(payload: Value, resolve: bool) -> Value {
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
        if resolve {
            if let (Some(id), Some(name)) = (payload["id"].as_str(), payload["name"].as_str())
                && valid_id(id)
                && valid_name(name)
            {
                return json!({"ok":true,"revision":revision,"id":id,"name":name});
            }
        } else if let Some(entries) = payload["entries"].as_array() {
            let projected: Option<Vec<Value>> = entries
                .iter()
                .map(|entry| {
                    let id = entry["id"].as_str()?;
                    let name = entry["name"].as_str()?;
                    (valid_id(id) && valid_name(name)).then(|| json!({"id":id,"name":name}))
                })
                .collect();
            let cursor = payload.get("next_cursor");
            if entries.len() <= 32
                && let (Some(entries), Some(cursor)) = (projected, cursor)
            {
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
        resolve: bool,
    },
    ArtworkResolve {
        revision: Option<String>,
        offset: u64,
    },
    ArtworkProvider {
        artwork: Artwork,
        revision: String,
        offset: u64,
    },
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
struct Library {
    runtime: RuntimeClient,
    admissions: BTreeMap<u64, Admission>,
    pending: BTreeMap<u64, Pending>,
    discovery: discovery::Settings,
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
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
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
                Stage::ArtworkResolve { revision, offset } if payload["ok"] == true => {
                    let artwork =
                        serde_json::from_value::<Artwork>(payload["artwork"].clone()).ok();
                    let provider = artwork
                        .as_ref()
                        .and_then(|a| a.request(revision.as_deref(), offset));
                    if let (Some(artwork), Some((service, request))) = (artwork, provider) {
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
                    pending.result = Some(error(
                        "artwork_unavailable",
                        "artwork unavailable or changed",
                    ));
                }
                Stage::ArtworkResolve { .. } => {
                    pending.result = Some(error("artwork_unavailable", "artwork unavailable"))
                }
                Stage::ArtworkProvider {
                    artwork,
                    revision,
                    offset,
                } => pending.result = Some(artwork_chunk(payload, &artwork, &revision, offset)),
                Stage::Catalog { resolve } => pending.result = Some(project(payload, resolve)),
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
                id,
                revision,
                offset,
            }) if valid_id(&id) && valid_artwork_cursor(revision.as_deref(), offset) => Ok((
                CATALOG.to_owned(),
                json!({"action":"resolve_exported_artwork","id":id}),
                Stage::ArtworkResolve { revision, offset },
            )),
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
                .map(|(payload, resolve)| (CATALOG.to_owned(), payload, Stage::Catalog { resolve }))
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
            artwork.request(Some("17"), 0).unwrap().0,
            "amaterasu.steam/1"
        );
        assert!(artwork.request(Some("18"), 0).is_none());
        assert!(artwork.request(None, 0).is_some());
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
            true,
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
            false,
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
                    payload: json!({"action":"list","limit":20}),
                    remaining_ms: 5000,
                }
            }
            Command::Poll {} => Response::Pending,
            Command::Request { .. } => Response::Accepted { id: 12 },
            Command::TakeResult { request: 12 } => Response::Result {
                payload: json!({"ok":true,"revision":4,"entries":[{"id":"app-0000000000000001","name":"Game","launch":"private"}],"next_cursor":null}),
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
}
