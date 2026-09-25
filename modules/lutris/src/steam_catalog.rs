//! Read a complete, stable Steam-ID catalog snapshot before suppressing Lutris duplicates.

use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};
use std::collections::BTreeSet;

use crate::configuration::release_terminal_child_result;

const SERVICE: &str = "amaterasu.catalog/1";
const TIMEOUT_MS: u64 = 30_000;
const PAGE_SIZE: usize = 32;
const MAX_STEAM_IDS: usize = 20_000;
const MAX_CATALOG_CURSOR_BYTES: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
enum Call {
    Submit { operation_id: String },
    Waiting { request: u64 },
    Delay { ticks: u8 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Progress {
    Pending,
    Complete(BTreeSet<u32>),
    Failed(String),
}

pub struct SteamCatalogRead {
    revision: Option<u64>,
    cursor: Option<String>,
    steam_ids: BTreeSet<u32>,
    call: Call,
    operation_sequence: u64,
}

impl SteamCatalogRead {
    pub fn new() -> Self {
        Self {
            revision: None,
            cursor: None,
            steam_ids: BTreeSet::new(),
            call: Call::Submit {
                operation_id: "lutris.steam-id-export.1".into(),
            },
            operation_sequence: 1,
        }
    }

    pub fn advance(&mut self, runtime: &RuntimeClient) -> ModuleResult<Progress> {
        match self.call.clone() {
            Call::Delay { ticks } if ticks > 1 => {
                self.call = Call::Delay { ticks: ticks - 1 };
                Ok(Progress::Pending)
            }
            Call::Delay { .. } => {
                self.call = Call::Submit {
                    operation_id: self.next_operation_id(),
                };
                Ok(Progress::Pending)
            }
            Call::Submit { operation_id } => match runtime.send(Command::Request {
                service: SERVICE.into(),
                payload: self.request_payload(),
                timeout_ms: TIMEOUT_MS,
                operation_id: operation_id.clone(),
                parent: None,
            }) {
                Ok(Response::Accepted { id }) => {
                    self.call = Call::Waiting { request: id };
                    Ok(Progress::Pending)
                }
                Ok(Response::Result { payload }) => Ok(self.consume_page(payload)),
                Ok(Response::Error { code, message }) => {
                    Ok(Progress::Failed(format!("{code}: {message}")))
                }
                Ok(response) => Ok(Progress::Failed(format!(
                    "unexpected Steam-ID catalog response: {response:?}"
                ))),
                Err(_) => {
                    self.call = Call::Delay { ticks: 2 };
                    Ok(Progress::Pending)
                }
            },
            Call::Waiting { request } => {
                let response = match runtime.send(Command::TakeResult { request }) {
                    Ok(Response::Pending) => return Ok(Progress::Pending),
                    Ok(response) => response,
                    Err(_) => return Ok(Progress::Pending),
                };
                let Some(response) = release_terminal_child_result(request, response, |request| {
                    runtime.send(Command::ReleaseResult { request })
                }) else {
                    return Ok(Progress::Pending);
                };
                match response {
                    Response::Result { payload } => Ok(self.consume_page(payload)),
                    Response::Error { code, message } => {
                        Ok(Progress::Failed(format!("{code}: {message}")))
                    }
                    other => Ok(Progress::Failed(format!(
                        "unexpected Steam-ID catalog response: {other:?}"
                    ))),
                }
            }
        }
    }

    fn request_payload(&self) -> Value {
        let mut payload = json!({
            "action":"export_steam_ids",
            "limit":PAGE_SIZE
        });
        if let (Some(revision), Some(after)) = (self.revision, self.cursor.as_ref()) {
            payload["cursor"] = json!({
                "revision":revision,
                "after":after,
                "query":"steam_ids"
            });
        }
        payload
    }

    fn consume_page(&mut self, payload: Value) -> Progress {
        self.try_consume_page(payload)
            .unwrap_or_else(Progress::Failed)
    }

    fn try_consume_page(&mut self, payload: Value) -> Result<Progress, String> {
        if payload.get("ok").and_then(Value::as_bool) != Some(true) {
            return Ok(Progress::Failed(
                "Steam-ID catalog export is unavailable".into(),
            ));
        }
        let revision = payload
            .get("revision")
            .and_then(Value::as_u64)
            .ok_or("Steam-ID catalog response omitted revision")?;
        if self.revision.is_some_and(|expected| expected != revision) {
            return Ok(Progress::Failed(
                "Steam-ID catalog changed during pagination".into(),
            ));
        }
        let steam_ids = payload
            .get("steam_ids")
            .and_then(Value::as_array)
            .ok_or("Steam-ID catalog response omitted steam_ids")?;
        if steam_ids.len() > PAGE_SIZE {
            return Ok(Progress::Failed(
                "Steam-ID catalog page exceeds its bound".into(),
            ));
        }
        let mut page_ids = BTreeSet::new();
        for steam_id in steam_ids {
            let steam_id = steam_id
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or("Steam-ID catalog contains an invalid Steam ID")?;
            page_ids.insert(steam_id);
        }
        if self.steam_ids.len() + page_ids.difference(&self.steam_ids).count() > MAX_STEAM_IDS {
            return Ok(Progress::Failed(
                "Steam-ID catalog export exceeds its bound".into(),
            ));
        }

        let next_cursor = payload
            .get("next_cursor")
            .filter(|value| !value.is_null())
            .map(|cursor| -> Result<String, String> {
                let cursor_revision = cursor
                    .get("revision")
                    .and_then(Value::as_u64)
                    .ok_or("Steam-ID catalog response has an invalid cursor revision")?;
                if cursor_revision != revision {
                    return Err("Steam-ID catalog cursor revision does not match its page".into());
                }
                let after = cursor
                    .get("after")
                    .and_then(Value::as_str)
                    .ok_or("Steam-ID catalog response has an invalid cursor key")?;
                if after.is_empty()
                    || after.len() > MAX_CATALOG_CURSOR_BYTES
                    || after.chars().any(char::is_control)
                {
                    return Err("Steam-ID catalog response has an invalid cursor key".into());
                }
                if cursor.get("query").and_then(Value::as_str) != Some("steam_ids") {
                    return Err("Steam-ID catalog response has an invalid cursor query".into());
                }
                Ok(after.to_owned())
            })
            .transpose()?;

        if let Some(next_after) = next_cursor {
            if self
                .cursor
                .as_ref()
                .is_some_and(|after| next_after.as_str() <= after.as_str())
            {
                return Ok(Progress::Failed(
                    "Steam-ID catalog cursor did not advance".into(),
                ));
            }

            self.revision = Some(revision);
            self.steam_ids.extend(page_ids);
            self.cursor = Some(next_after);
            self.call = Call::Submit {
                operation_id: self.next_operation_id(),
            };
            Ok(Progress::Pending)
        } else {
            self.revision = Some(revision);
            self.steam_ids.extend(page_ids);
            Ok(Progress::Complete(self.steam_ids.clone()))
        }
    }

    fn next_operation_id(&mut self) -> String {
        self.operation_sequence = self.operation_sequence.saturating_add(1);
        format!("lutris.steam-id-export.{}", self.operation_sequence)
    }
}

impl Default for SteamCatalogRead {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_requests_pin_lexical_pages_and_deduplicate_steam_ids() {
        let mut scan = SteamCatalogRead::new();
        assert_eq!(SERVICE, "amaterasu.catalog/1");
        assert_eq!(
            scan.request_payload(),
            json!({"action":"export_steam_ids","limit":32})
        );

        let first = scan.consume_page(json!({
            "ok":true,"revision":4,
            "steam_ids":[10,11],
            "next_cursor":{"revision":4,"after":"entry/11","query":"steam_ids"}
        }));
        assert_eq!(first, Progress::Pending);
        assert_eq!(scan.cursor.as_deref(), Some("entry/11"));
        assert_eq!(
            scan.request_payload(),
            json!({
                "action":"export_steam_ids",
                "limit":32,
                "cursor":{"revision":4,"after":"entry/11","query":"steam_ids"}
            })
        );
        assert_eq!(
            scan.consume_page(json!({
                "ok":true,"revision":4,
                "steam_ids":[11,15],"next_cursor":null
            })),
            Progress::Complete([10, 11, 15].into())
        );

        let mut omitted_cursor = SteamCatalogRead::new();
        assert_eq!(
            omitted_cursor.consume_page(json!({"ok":true,"revision":8,"steam_ids":[42]})),
            Progress::Complete([42].into())
        );
    }

    #[test]
    fn export_rejects_invalid_ids_and_unstable_or_malformed_cursors() {
        let malformed_pages = [
            json!({"ok":false,"revision":4,"steam_ids":[],"next_cursor":null}),
            json!({"ok":true,"steam_ids":[],"next_cursor":null}),
            json!({"ok":true,"revision":4,"games":[],"next_cursor":null}),
            json!({"ok":true,"revision":4,"steam_ids":[0],"next_cursor":null}),
            json!({"ok":true,"revision":4,"steam_ids":[4294967296_u64],"next_cursor":null}),
            json!({"ok":true,"revision":4,"steam_ids":["10"],"next_cursor":null}),
            json!({
                "ok":true,"revision":4,"steam_ids":[],
                "next_cursor":{"revision":5,"after":"entry/1","query":"steam_ids"}
            }),
            json!({
                "ok":true,"revision":4,"steam_ids":[],
                "next_cursor":{"revision":4,"after":"entry/1","query":"other"}
            }),
            json!({
                "ok":true,"revision":4,"steam_ids":[],
                "next_cursor":{"revision":4,"after":10,"query":"steam_ids"}
            }),
            json!({
                "ok":true,"revision":4,"steam_ids":[],
                "next_cursor":{"revision":4,"after":"","query":"steam_ids"}
            }),
            json!({
                "ok":true,"revision":4,"steam_ids":[],
                "next_cursor":{"revision":4,"after":"x".repeat(MAX_CATALOG_CURSOR_BYTES + 1),"query":"steam_ids"}
            }),
            json!({
                "ok":true,"revision":4,"steam_ids":[],
                "next_cursor":{"revision":4,"after":"entry\nkey","query":"steam_ids"}
            }),
        ];
        for malformed in malformed_pages {
            let mut scan = SteamCatalogRead::new();
            assert!(scan.consume_page(malformed).is_failed());
        }

        let oversized_page = json!({
            "ok":true,
            "revision":4,
            "steam_ids":(1..=PAGE_SIZE + 1).collect::<Vec<_>>(),
            "next_cursor":null
        });
        assert!(
            SteamCatalogRead::new()
                .consume_page(oversized_page)
                .is_failed()
        );

        let mut maximum_cursor = SteamCatalogRead::new();
        assert_eq!(
            maximum_cursor.consume_page(json!({
                "ok":true,
                "revision":4,
                "steam_ids":[],
                "next_cursor":{
                    "revision":4,
                    "after":"x".repeat(MAX_CATALOG_CURSOR_BYTES),
                    "query":"steam_ids"
                }
            })),
            Progress::Pending
        );

        let mut provider_marker_cursor = SteamCatalogRead::new();
        let provider_marker = "~provider.entry:amaterasu.lutris:steam game/é";
        assert_eq!(
            provider_marker_cursor.consume_page(json!({
                "ok":true,
                "revision":4,
                "steam_ids":[],
                "next_cursor":{
                    "revision":4,
                    "after":provider_marker,
                    "query":"steam_ids"
                }
            })),
            Progress::Pending
        );
        assert_eq!(
            provider_marker_cursor.cursor.as_deref(),
            Some(provider_marker)
        );

        let mut full_catalog = SteamCatalogRead::new();
        full_catalog.steam_ids.extend(1..=MAX_STEAM_IDS as u32);
        assert!(
            full_catalog
                .consume_page(json!({
                    "ok":true,
                    "revision":4,
                    "steam_ids":[MAX_STEAM_IDS as u32 + 1],
                    "next_cursor":null
                }))
                .is_failed()
        );

        for after in ["entry/10", "entry/09"] {
            let mut scan = SteamCatalogRead::new();
            scan.revision = Some(4);
            scan.cursor = Some("entry/10".into());
            assert!(
                scan.consume_page(json!({
                    "ok":true,"revision":4,"steam_ids":[],
                    "next_cursor":{"revision":4,"after":after,"query":"steam_ids"}
                }))
                .is_failed()
            );
        }

        let mut changed_catalog = SteamCatalogRead::new();
        changed_catalog.revision = Some(4);
        changed_catalog.cursor = Some("entry/10".into());
        assert!(
            changed_catalog
                .consume_page(json!({"ok":true,"revision":5,"steam_ids":[],"next_cursor":null}))
                .is_failed()
        );
    }

    impl Progress {
        fn is_failed(&self) -> bool {
            matches!(self, Self::Failed(_))
        }
    }
}
