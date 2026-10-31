//! Client-facing view of the configured catalog export.
//!
//! The catalog remains the source of truth and the launch service retains
//! ownership of launch operations and their retirement proofs.
mod discovery;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

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
            Self::Discover { .. } => Err("discovery is not a catalog request"),
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

enum Projection {
    Catalog(bool),
    Discovery(usize),
}

struct Pending {
    parent: u64,
    projection: Projection,
    result: Option<Value>,
    released: bool,
}
struct Library {
    runtime: RuntimeClient,
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
            pending: BTreeMap::new(),
            discovery,
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        let mut finished = Vec::new();
        for (&child, pending) in &mut self.pending {
            if pending.result.is_none() {
                match self.runtime.send(Command::TakeResult { request: child }) {
                    Ok(Response::Result { payload }) => {
                        pending.result = Some(match pending.projection {
                            Projection::Catalog(resolve) => project(payload, resolve),
                            Projection::Discovery(limit) => discovery::project(payload, limit),
                        })
                    }
                    Ok(Response::Pending) | Err(_) => continue,
                    _ => {
                        pending.result = Some(match pending.projection {
                            Projection::Catalog(_) => {
                                error("catalog_unavailable", "catalog request failed")
                            }
                            Projection::Discovery(_) => {
                                error("provider_unavailable", "discovery provider request failed")
                            }
                        })
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
            if pending.released
                && matches!(
                    self.runtime.send(Command::Complete {
                        request: pending.parent,
                        payload: pending.result.as_ref().unwrap().clone()
                    }),
                    Ok(Response::Ok) | Ok(Response::Error { .. })
                )
            {
                finished.push(child);
            }
        }
        for child in finished {
            self.pending.remove(&child);
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
        } else if self.pending.len() >= MAX_PENDING {
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
        let route = serde_json::from_value::<Action>(payload)
            .map_err(|_| ("invalid_request", "invalid request"))
            .and_then(|action| match action {
                Action::Discover {
                    provider,
                    cursor,
                    limit,
                } => self
                    .discovery
                    .route(discovery::Request::new(provider, cursor, limit), &initiator)
                    .map(|(service, payload, limit)| {
                        (service, payload, Projection::Discovery(limit))
                    })
                    .map_err(|code| (code, "discovery request rejected")),
                action => action
                    .catalog_request()
                    .map(|(payload, resolve)| {
                        (CATALOG.into(), payload, Projection::Catalog(resolve))
                    })
                    .map_err(|message| ("invalid_request", message)),
            });
        let (service, payload, projection) = match route {
            Ok(value) => value,
            Err((code, message)) => {
                let _ = self.runtime.send_ephemeral(Command::Complete {
                    request: id,
                    payload: error(code, message),
                });
                return Ok(());
            }
        };
        let unavailable = match projection {
            Projection::Catalog(_) => (
                "catalog_unavailable",
                "catalog request could not be admitted",
            ),
            Projection::Discovery(_) => (
                "provider_unavailable",
                "discovery provider request could not be admitted",
            ),
        };
        match self.runtime.send_ephemeral(Command::Request {
            service,
            payload,
            timeout_ms: remaining_ms.min(30_000),
            operation_id: format!("library.catalog.{id}"),
            parent: Some(id),
        }) {
            Ok(Response::Accepted { id: child }) => {
                self.pending.insert(
                    child,
                    Pending {
                        parent: id,
                        projection,
                        result: None,
                        released: false,
                    },
                );
            }
            _ => {
                let _ = self.runtime.send_ephemeral(Command::Complete {
                    request: id,
                    payload: error(unavailable.0, unavailable.1),
                });
            }
        }
        Ok(())
    }
}
amaterasu_module_sdk::export_module!(Library);

#[cfg(test)]
mod tests {
    use super::*;
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
