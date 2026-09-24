//! Read a complete, stable Steam provider snapshot before suppressing Lutris duplicates.

use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};
use std::collections::BTreeSet;

use crate::configuration::release_terminal_child_result;

const SERVICE: &str = "amaterasu.steam/1";
const TIMEOUT_MS: u64 = 30_000;
const PAGE_SIZE: usize = 32;
const MAX_GAMES: usize = 20_000;

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
    cursor: Option<u32>,
    app_ids: BTreeSet<u32>,
    call: Call,
    operation_sequence: u64,
    stop_after_call: bool,
}

impl SteamCatalogRead {
    pub fn new() -> Self {
        let mut value = Self {
            revision: None,
            cursor: None,
            app_ids: BTreeSet::new(),
            call: Call::Submit {
                operation_id: "lutris.steam-list.1".into(),
            },
            operation_sequence: 1,
            stop_after_call: false,
        };
        value.cursor = Some(0);
        value
    }

    pub fn cancel(&mut self) {
        self.stop_after_call = true;
    }

    pub fn advance(&mut self, runtime: &RuntimeClient) -> ModuleResult<Progress> {
        if self.stop_after_call {
            match self.call {
                Call::Submit { .. } | Call::Delay { .. } => {
                    return Ok(Progress::Failed("cancelled_after_settings_change".into()));
                }
                Call::Waiting { .. } => {}
            }
        }
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
                    "unexpected Steam list response: {response:?}"
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
                        "unexpected Steam list response: {other:?}"
                    ))),
                }
            }
        }
    }

    fn request_payload(&self) -> Value {
        json!({
            "action":"list",
            "revision":self.revision,
            "after":self.cursor.unwrap_or(0),
            "limit":PAGE_SIZE
        })
    }

    fn consume_page(&mut self, payload: Value) -> Progress {
        self.try_consume_page(payload)
            .unwrap_or_else(Progress::Failed)
    }

    fn try_consume_page(&mut self, payload: Value) -> Result<Progress, String> {
        if payload.get("ok").and_then(Value::as_bool) != Some(true)
            || payload.get("complete").and_then(Value::as_bool) != Some(true)
        {
            return Ok(Progress::Failed(
                "Steam catalog is unavailable or incomplete".into(),
            ));
        }
        let revision = payload
            .get("revision")
            .and_then(Value::as_u64)
            .ok_or("Steam list response omitted revision")?;
        if self.revision.is_some_and(|expected| expected != revision) {
            return Ok(Progress::Failed(
                "Steam catalog changed during pagination".into(),
            ));
        }
        self.revision = Some(revision);
        let games = payload
            .get("games")
            .and_then(Value::as_array)
            .ok_or("Steam list response omitted games")?;
        if games.len() > PAGE_SIZE {
            return Ok(Progress::Failed("Steam list page exceeds its bound".into()));
        }
        for game in games {
            let app_id = game
                .get("app_id")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or("Steam list contains an invalid app ID")?;
            if !self.app_ids.insert(app_id) || self.app_ids.len() > MAX_GAMES {
                return Ok(Progress::Failed(
                    "Steam list contains duplicates or exceeds its bound".into(),
                ));
            }
        }
        let next_after = payload
            .get("next_after")
            .filter(|value| !value.is_null())
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or("Steam list response has an invalid cursor")
            })
            .transpose()?;
        if let Some(next_after) = next_after {
            if Some(next_after) <= self.cursor {
                return Ok(Progress::Failed("Steam list cursor did not advance".into()));
            }
            self.cursor = Some(next_after);
            self.call = Call::Submit {
                operation_id: self.next_operation_id(),
            };
            Ok(Progress::Pending)
        } else if self.stop_after_call {
            Ok(Progress::Failed("cancelled_after_settings_change".into()))
        } else {
            Ok(Progress::Complete(self.app_ids.clone()))
        }
    }

    fn next_operation_id(&mut self) -> String {
        self.operation_sequence = self.operation_sequence.saturating_add(1);
        format!("lutris.steam-list.{}", self.operation_sequence)
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
    fn steam_pagination_requires_complete_stable_unique_app_ids() {
        let mut scan = SteamCatalogRead::new();
        let first = scan.consume_page(json!({
            "ok":true,"complete":true,"revision":4,
            "games":[{"app_id":10},{"app_id":11}],"next_after":11
        }));
        assert_eq!(first, Progress::Pending);
        assert_eq!(scan.cursor, Some(11));
        assert_eq!(
            scan.consume_page(json!({
                "ok":true,"complete":true,"revision":4,
                "games":[{"app_id":15}],"next_after":null
            })),
            Progress::Complete([10, 11, 15].into())
        );

        for malformed in [
            json!({"ok":true,"complete":false,"revision":4,"games":[],"next_after":null}),
            json!({"ok":true,"complete":true,"revision":5,"games":[],"next_after":null}),
            json!({"ok":true,"complete":true,"revision":4,"games":[{"app_id":0}],"next_after":null}),
        ] {
            let mut scan = SteamCatalogRead::new();
            scan.revision = Some(4);
            assert!(scan.consume_page(malformed).is_failed());
        }
    }

    impl Progress {
        fn is_failed(&self) -> bool {
            matches!(self, Self::Failed(_))
        }
    }
}
