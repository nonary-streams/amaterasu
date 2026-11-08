//! Keep imported artwork references current without enabling catalog auto-sync.
use crate::Provider;
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

// The broker retains operation IDs for five minutes. Never replay an unknown
// admission after that horizon, when it could become a second operation.
const ADMISSION_REPLAY_LIMIT: Duration = Duration::from_secs(300);

struct Pending {
    command: Command,
    started: Instant,
    request: Option<u64>,
    observed: Option<Value>,
    uncertain: bool,
    app_id: u32,
    revision: String,
}

#[derive(Default)]
pub(super) struct Refresh {
    completed: BTreeMap<u32, String>,
    pending: Option<Pending>,
    sequence: u64,
    retry_at: Option<Instant>,
}

impl Refresh {
    pub(super) fn tick(
        &mut self,
        runtime: &RuntimeClient,
        provider: &Provider,
    ) -> ModuleResult<()> {
        if !self.advance(&mut |command| runtime.send(command), false)? {
            return Ok(());
        }
        if !provider.enabled
            || provider.scan_in_progress()
            || !provider.has_artwork_snapshot()
            || self
                .retry_at
                .is_some_and(|deadline| Instant::now() < deadline)
        {
            return Ok(());
        }
        self.completed
            .retain(|id, _| provider.games.contains_key(id));
        let next = provider.games.iter().find_map(|(&app_id, game)| {
            let revision = game.artwork_revision.as_ref()?;
            (self.completed.get(&app_id) != Some(revision)).then(|| (app_id, revision.clone()))
        });
        let Some((app_id, revision)) = next else {
            return Ok(());
        };
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("artwork refresh sequence exhausted")?;
        let command = Command::Request {
            service: "amaterasu.catalog/1".into(),
            payload: json!({"action":"provider_artwork_refresh","source_id":format!("steam:{app_id}"),
                "artwork":{"provider":"steam","app_id":app_id,"revision":revision}}),
            timeout_ms: 5_000,
            operation_id: format!("steam.artwork-refresh.{}", self.sequence),
            parent: None,
        };
        self.pending = Some(Pending {
            command,
            started: Instant::now(),
            request: None,
            observed: None,
            uncertain: false,
            app_id,
            revision,
        });
        self.advance(&mut |command| runtime.send(command), false)?;
        Ok(())
    }

    /// Retain uncertain admissions and delivered writes until a terminal result
    /// is observed and its broker record is released. Cancellation is not proof
    /// that a catalog mutation has stopped.
    pub(super) fn poll_shutdown(&mut self, runtime: &RuntimeClient) -> bool {
        self.advance(&mut |command| runtime.send(command), true)
            .unwrap_or(false)
    }

    fn advance(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
        shutdown: bool,
    ) -> ModuleResult<bool> {
        self.advance_with_clock(send, shutdown, &mut Instant::now)
    }

    fn advance_with_clock(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
        shutdown: bool,
        clock: &mut impl FnMut() -> Instant,
    ) -> ModuleResult<bool> {
        let Some(pending) = self.pending.as_mut() else {
            return Ok(true);
        };
        if pending.request.is_none() {
            if pending.uncertain
                && clock().saturating_duration_since(pending.started) >= ADMISSION_REPLAY_LIMIT
            {
                return Ok(false);
            }
            let command = if shutdown {
                let Command::Request {
                    service,
                    payload,
                    operation_id,
                    parent,
                    ..
                } = &pending.command
                else {
                    return Ok(false);
                };
                Command::LookupRequest {
                    service: service.clone(),
                    payload: payload.clone(),
                    operation_id: operation_id.clone(),
                    parent: *parent,
                }
            } else {
                pending.command.clone()
            };
            match send(command) {
                Ok(Response::Accepted { id }) => pending.request = Some(id),
                Ok(Response::Result { payload }) if !shutdown => pending.observed = Some(payload),
                Ok(Response::Error { code, .. })
                    if shutdown
                        && code == "not_found"
                        && clock().saturating_duration_since(pending.started)
                            < ADMISSION_REPLAY_LIMIT =>
                {
                    self.pending = None;
                    return Ok(true);
                }
                Ok(Response::Error { code, .. })
                    if !pending.uncertain
                        && !shutdown
                        && matches!(
                            code.as_str(),
                            "permission_denied"
                                | "invalid_request"
                                | "unavailable"
                                | "resource_exhausted"
                        ) =>
                {
                    self.pending = None;
                    self.retry_at = Some(Instant::now() + Duration::from_secs(5));
                    return Ok(true);
                }
                _ => {
                    pending.uncertain = true;
                    return Ok(false);
                }
            }
        }
        if let Some(request) = pending.request {
            if pending.observed.is_none() {
                match send(Command::TakeResult { request }) {
                    Ok(Response::Result { payload }) => pending.observed = Some(payload),
                    // A failed, cancelled or expired broker result does not prove
                    // that a delivered native provider effect has finished.
                    _ => return Ok(false),
                }
            }
            match send(Command::ReleaseResult { request }) {
                Ok(Response::Ok) => {}
                Ok(Response::Error { code, .. }) if code == "not_found" => {}
                _ => return Ok(false),
            }
        }
        let Some(payload) = pending.observed.take() else {
            return Ok(false);
        };
        let app_id = pending.app_id;
        let revision = pending.revision.clone();
        self.complete(app_id, revision, payload);
        Ok(true)
    }

    fn complete(&mut self, app_id: u32, revision: String, payload: Value) {
        self.pending = None;
        if payload["ok"] == true || payload["error"]["code"] == "provider_artwork_required" {
            self.completed.insert(app_id, revision);
            self.retry_at = None;
        } else {
            self.retry_at = Some(Instant::now() + Duration::from_secs(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncertain_write_keeps_identity_and_waits_for_terminal_release() {
        let command = Command::Request {
            service: "amaterasu.catalog/1".into(),
            payload: json!({"action":"provider_artwork_refresh"}),
            timeout_ms: 5000,
            operation_id: "steam.artwork-refresh.1".into(),
            parent: None,
        };
        let mut refresh = Refresh {
            pending: Some(Pending {
                command: command.clone(),
                started: Instant::now(),
                request: None,
                observed: None,
                uncertain: false,
                app_id: 42,
                revision: "r".into(),
            }),
            ..Default::default()
        };
        assert!(
            !refresh
                .advance(&mut |_| Err("lost admission".into()), false)
                .unwrap()
        );
        assert!(
            !refresh
                .advance(&mut |retry| {
                    if matches!(retry, Command::TakeResult { request: 9 }) {
                        Ok(Response::Pending)
                    } else {
                        assert!(matches!(retry, Command::LookupRequest { operation_id, .. } if operation_id == "steam.artwork-refresh.1"));
                        Ok(Response::Accepted { id: 9 })
                    }
                }, true)
                .unwrap()
        );
        assert!(
            !refresh
                .advance(
                    &mut |command| {
                        assert!(matches!(command, Command::TakeResult { request: 9 }));
                        Ok(Response::Pending)
                    },
                    true
                )
                .unwrap()
        );
        let mut calls = 0;
        assert!(
            !refresh
                .advance(
                    &mut |command| {
                        calls += 1;
                        match command {
                            Command::TakeResult { request: 9 } => Ok(Response::Result {
                                payload: json!({"ok": true}),
                            }),
                            Command::ReleaseResult { request: 9 } => Err("lost release".into()),
                            other => panic!("unexpected command: {other:?}"),
                        }
                    },
                    true
                )
                .unwrap()
        );
        assert_eq!(calls, 2);
        assert!(
            refresh
                .advance(
                    &mut |command| {
                        assert!(matches!(command, Command::ReleaseResult { request: 9 }));
                        Ok(Response::Error {
                            code: "not_found".into(),
                            message: "released".into(),
                        })
                    },
                    true
                )
                .unwrap()
        );
        assert_eq!(refresh.completed.get(&42).map(String::as_str), Some("r"));
    }

    #[test]
    fn expired_unknown_admission_is_never_replayed() {
        let mut refresh = Refresh {
            pending: Some(Pending {
                command: Command::Poll {},
                started: Instant::now() - ADMISSION_REPLAY_LIMIT,
                request: None,
                observed: None,
                uncertain: true,
                app_id: 1,
                revision: "r".into(),
            }),
            ..Default::default()
        };
        assert!(
            !refresh
                .advance(&mut |_| panic!("must not replay expired admission"), false)
                .unwrap()
        );
    }

    #[test]
    fn missing_lookup_retires_only_inside_broker_retention_window() {
        let now = Instant::now();
        let command = Command::Request {
            service: "amaterasu.catalog/1".into(),
            payload: json!({"action": "provider_artwork_refresh"}),
            timeout_ms: 5_000,
            operation_id: "steam.artwork-refresh.2".into(),
            parent: None,
        };
        let mut refresh = Refresh {
            pending: Some(Pending {
                command: command.clone(),
                started: now,
                request: None,
                observed: None,
                uncertain: true,
                app_id: 1,
                revision: "r".into(),
            }),
            ..Default::default()
        };
        assert!(
            refresh
                .advance_with_clock(
                    &mut |command| {
                        assert!(matches!(command, Command::LookupRequest { .. }));
                        Ok(Response::Error {
                            code: "not_found".into(),
                            message: "missing".into(),
                        })
                    },
                    true,
                    &mut || now + Duration::from_secs(299),
                )
                .unwrap()
        );
        assert!(refresh.pending.is_none());
        refresh.pending = Some(Pending {
            command,
            started: now,
            request: None,
            observed: None,
            uncertain: true,
            app_id: 1,
            revision: "r".into(),
        });
        let mut times = [
            now + Duration::from_secs(299),
            now + Duration::from_secs(300),
        ]
        .into_iter();
        assert!(
            !refresh
                .advance_with_clock(
                    &mut |_| Ok(Response::error("not_found", "expired during lookup")),
                    true,
                    &mut || times.next().unwrap(),
                )
                .unwrap()
        );
        assert!(refresh.pending.is_some());
    }
}
