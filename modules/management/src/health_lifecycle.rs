//! Retire every broker child used by the management operational-health probe.
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

// Broker terminal request records live for at least this long. Measure from
// before the first possible admission, so a missing lookup inside this window
// proves that the request was never admitted (and was not explicitly released).
const REQUEST_RETENTION: Duration = Duration::from_secs(300);

#[derive(Clone, Copy)]
enum Probe {
    Authentication,
    Catalog,
    Pairing,
}

enum Child {
    Submit {
        command: Command,
        uncertain: bool,
        first_sent_at: Option<Instant>,
    },
    Waiting(u64),
    Release {
        id: u64,
        healthy: bool,
    },
    CancelRead {
        id: u64,
        cancelled: bool,
    },
    None,
}

pub(crate) struct OperationalHealth {
    parent: u64,
    deadline: Instant,
    probe: Probe,
    child: Child,
    finishing: Option<Value>,
    parent_finished: bool,
}

impl OperationalHealth {
    pub(crate) fn new(parent: u64, remaining_ms: u64, now: Instant) -> Self {
        let deadline = now + Duration::from_millis(remaining_ms.min(10_000));
        Self {
            parent,
            deadline,
            probe: Probe::Authentication,
            child: Child::None,
            finishing: None,
            parent_finished: false,
        }
    }

    /// Returns true only after every admitted child and the parent response are retired.
    pub(crate) fn advance(
        &mut self,
        now: Instant,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> bool {
        if now >= self.deadline && self.finishing.is_none() {
            self.finishing = Some(json!({"ok":false,"code":"health_timeout"}));
        }
        self.step(now, send)
    }

    /// Stop issuing new probes and await provider completion and result release.
    pub(crate) fn quiesce(
        &mut self,
        now: Instant,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> bool {
        if self.finishing.is_none() {
            self.finishing = Some(json!({"ok":false,"code":"shutting_down"}));
        }
        self.step(now, send)
    }

    fn step(
        &mut self,
        now: Instant,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> bool {
        let child = std::mem::replace(&mut self.child, Child::None);
        match child {
            Child::None if self.finishing.is_none() => {
                self.child = Child::Submit {
                    command: self.request(now),
                    uncertain: false,
                    first_sent_at: None,
                };
            }
            Child::None => {
                if self.parent_finished || self.finish_parent(send) {
                    return true;
                }
            }
            Child::Submit {
                command,
                uncertain,
                first_sent_at,
            } => {
                if self.finishing.is_some() && !uncertain {
                    return false;
                }
                let lookup = self.finishing.is_some();
                let sent = if lookup {
                    lookup_request(&command)
                } else {
                    command.clone()
                };
                match send(sent) {
                    Ok(Response::Accepted { id }) => {
                        self.child = Child::Waiting(id);
                    }
                    Ok(Response::Error { code, .. }) if !uncertain && code != "conflict" => {
                        if self.finishing.is_none() {
                            self.finishing =
                                Some(json!({"ok":false,"code":"required_service_unavailable"}));
                        }
                    }
                    Ok(Response::Error { code, .. })
                        if lookup
                            && code == "not_found"
                            && first_sent_at.is_some_and(|started| {
                                now.saturating_duration_since(started) < REQUEST_RETENTION
                            }) => {}
                    response => {
                        // A conflict can identify a pre-existing, different
                        // operation, so its age cannot establish absence.
                        let first_sent_at = if !lookup
                            && first_sent_at.is_none()
                            && !matches!(response, Ok(Response::Error { .. }))
                        {
                            Some(now)
                        } else {
                            first_sent_at
                        };
                        self.child = Child::Submit {
                            command,
                            uncertain: true,
                            first_sent_at,
                        }
                    }
                }
            }
            Child::Waiting(id) => {
                if self.finishing.is_some() && self.probe.is_read_only() {
                    self.child = Child::CancelRead {
                        id,
                        cancelled: false,
                    };
                    return false;
                }
                match send(Command::TakeResult { request: id }) {
                    Ok(Response::Result { payload }) => {
                        self.child = Child::Release {
                            id,
                            healthy: self.healthy(&payload),
                        };
                    }
                    // Broker timeouts, cancellations and missing records do not
                    // prove the provider stopped. Keep its identity fenced.
                    Ok(Response::Error { .. }) if self.probe.is_read_only() => {
                        self.finishing.get_or_insert_with(
                            || json!({"ok":false,"code":"required_service_unavailable"}),
                        );
                        self.child = Child::CancelRead {
                            id,
                            cancelled: false,
                        };
                    }
                    _ => self.child = Child::Waiting(id),
                }
            }
            Child::Release { id, healthy } => {
                if retired(send(Command::ReleaseResult { request: id })) {
                    if self.finishing.is_none() {
                        if !healthy {
                            self.finishing =
                                Some(json!({"ok":false,"code":"required_state_unavailable"}));
                        } else if let Some(next) = self.next_probe() {
                            self.probe = next;
                        } else {
                            self.finishing = Some(
                                json!({"ok":true,"authentication_verifier":true,"catalog_loaded":true,"pairing_identity":true}),
                            );
                        }
                    }
                } else {
                    self.child = Child::Release { id, healthy };
                }
            }
            Child::CancelRead { id, cancelled } => {
                if !cancelled {
                    if retired(send(Command::Cancel { request: id })) {
                        self.child = Child::CancelRead {
                            id,
                            cancelled: true,
                        };
                    } else {
                        self.child = Child::CancelRead { id, cancelled };
                    }
                } else if !retired(send(Command::ReleaseResult { request: id })) {
                    self.child = Child::CancelRead { id, cancelled };
                }
            }
        }
        false
    }

    fn finish_parent(&mut self, send: &mut impl FnMut(Command) -> ModuleResult<Response>) -> bool {
        let Some(payload) = self.finishing.clone() else {
            return false;
        };
        let finished = match send(Command::Complete {
            request: self.parent,
            payload,
        }) {
            Ok(Response::Ok) => true,
            Ok(Response::Error { code, .. }) if code == "not_found" || code == "request_ended" => {
                true
            }
            _ => false,
        };
        self.parent_finished = finished;
        finished
    }

    fn request(&self, now: Instant) -> Command {
        let (service, payload, name) = match self.probe {
            Probe::Authentication => (
                "amaterasu.authentication/1",
                json!({"action":"status"}),
                "authentication",
            ),
            Probe::Catalog => (
                "amaterasu.catalog/1",
                json!({"action":"list","cursor":null,"limit":1}),
                "catalog",
            ),
            Probe::Pairing => (
                "amaterasu.pairing/1",
                json!({"action":"identity"}),
                "pairing",
            ),
        };
        Command::Request {
            service: service.into(),
            payload,
            timeout_ms: self
                .deadline
                .saturating_duration_since(now)
                .as_millis()
                .max(1) as u64,
            operation_id: format!("management.health.{}.{name}", self.parent),
            parent: Some(self.parent),
        }
    }

    fn healthy(&self, payload: &Value) -> bool {
        payload.get("ok").and_then(Value::as_bool) == Some(true)
            && match self.probe {
                Probe::Authentication => {
                    payload.get("verifier_available").and_then(Value::as_bool) == Some(true)
                }
                Probe::Catalog => payload.get("entries").and_then(Value::as_array).is_some(),
                Probe::Pairing => payload.get("host_id").and_then(Value::as_str).is_some(),
            }
    }

    fn next_probe(&self) -> Option<Probe> {
        match self.probe {
            Probe::Authentication => Some(Probe::Catalog),
            Probe::Catalog => Some(Probe::Pairing),
            Probe::Pairing => None,
        }
    }
}

impl Probe {
    fn is_read_only(self) -> bool {
        // Catalog list can migrate legacy storage and authentication status
        // can prune sessions. Pairing identity only reads its stored identity.
        matches!(self, Self::Pairing)
    }
}

fn retired(response: ModuleResult<Response>) -> bool {
    matches!(response, Ok(Response::Ok))
        || matches!(response, Ok(Response::Error { code, .. }) if code == "not_found")
}

fn lookup_request(command: &Command) -> Command {
    let Command::Request {
        service,
        payload,
        operation_id,
        parent,
        ..
    } = command
    else {
        unreachable!("health probes only submit ordinary broker requests")
    };
    Command::LookupRequest {
        service: service.clone(),
        payload: payload.clone(),
        operation_id: operation_id.clone(),
        parent: *parent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_each_result_before_starting_the_next_probe() {
        let now = Instant::now();
        let mut check = OperationalHealth::new(7, 5_000, now);
        let mut commands = Vec::new();
        let mut releases = 0;
        let mut next_id = 0;
        let mut send = |command: Command| {
            commands.push(command.clone());
            Ok(match command {
                Command::Request { .. } => {
                    next_id += 1;
                    Response::Accepted { id: next_id }
                }
                Command::TakeResult { request } => Response::Result {
                    payload: match request {
                        1 => json!({"ok":true,"verifier_available":true}),
                        2 => json!({"ok":true,"entries":[]}),
                        _ => json!({"ok":true,"host_id":"host"}),
                    },
                },
                Command::ReleaseResult { .. } => {
                    releases += 1;
                    if releases == 1 {
                        return Err("transient release".into());
                    }
                    Response::Ok
                }
                Command::Complete { .. } => Response::Ok,
                _ => panic!("unexpected command"),
            })
        };
        for _ in 0..30 {
            if check.advance(now, &mut send) {
                break;
            }
        }
        drop(send);
        assert!(check.parent_finished);
        let requests = commands
            .iter()
            .filter(|c| matches!(c, Command::Request { .. }))
            .count();
        assert_eq!(requests, 3);
        assert_eq!(releases, 4);
        let first_catalog = commands.iter().position(|c| matches!(c, Command::Request { service, .. } if service == "amaterasu.catalog/1")).unwrap();
        let first_releases = commands[..first_catalog]
            .iter()
            .filter(|c| matches!(c, Command::ReleaseResult { request: 1 }))
            .count();
        assert_eq!(first_releases, 2);
    }

    #[test]
    fn timeout_retires_a_verified_read_only_probe() {
        let now = Instant::now();
        let mut check = OperationalHealth::new(7, 1, now);
        check.probe = Probe::Pairing;
        check.advance(now, &mut |_| panic!("first tick only prepares command"));
        check.advance(now, &mut |_| Ok(Response::Accepted { id: 9 }));
        let expired = now + Duration::from_millis(2);
        let mut commands = Vec::new();
        let mut cancel_attempts = 0;
        let mut release_attempts = 0;
        let mut send = |command: Command| {
            commands.push(command.clone());
            match command {
                Command::Cancel { .. } => {
                    cancel_attempts += 1;
                    if cancel_attempts == 1 {
                        Err("lost cancel".into())
                    } else {
                        Ok(Response::Ok)
                    }
                }
                Command::ReleaseResult { .. } => {
                    release_attempts += 1;
                    if release_attempts == 1 {
                        Err("lost release".into())
                    } else {
                        Ok(Response::Ok)
                    }
                }
                Command::Complete { .. } => Ok(Response::Ok),
                _ => panic!("unexpected command"),
            }
        };
        for _ in 0..10 {
            if check.advance(expired, &mut send) {
                break;
            }
        }
        drop(send);
        assert!(check.parent_finished);
        assert_eq!(cancel_attempts, 2);
        assert_eq!(release_attempts, 2);
        assert!(matches!(commands.last(), Some(Command::Complete { .. })));
    }

    #[test]
    fn shutdown_recovers_lost_child_before_completing_parent() {
        let now = Instant::now();
        let mut check = OperationalHealth::new(7, 5_000, now);
        check.probe = Probe::Pairing;
        check.advance(now, &mut |_| panic!("first tick only prepares command"));
        let mut original = None;
        check.advance(now, &mut |command| {
            original = Some(command);
            Err("lost admission".into())
        });
        let mut commands = Vec::new();
        let mut send = |command: Command| {
            commands.push(command.clone());
            Ok(match command {
                Command::Complete { .. } => Response::Ok,
                Command::LookupRequest { .. } => Response::Accepted { id: 9 },
                Command::Cancel { .. } => Response::Ok,
                Command::ReleaseResult { .. } => Response::Ok,
                _ => panic!("unexpected command"),
            })
        };
        for _ in 0..10 {
            if check.quiesce(now, &mut send) {
                break;
            }
        }
        drop(send);
        assert!(check.parent_finished);
        assert!(matches!(commands.last(), Some(Command::Complete { .. })));
        assert_eq!(
            serde_json::to_value(&commands[0]).unwrap(),
            serde_json::to_value(lookup_request(&original.unwrap())).unwrap()
        );
        assert!(
            commands
                .iter()
                .any(|c| matches!(c, Command::ReleaseResult { request: 9 }))
        );
    }

    #[test]
    fn timeout_keeps_possible_auth_pruning_or_catalog_migration_fenced() {
        let now = Instant::now();
        let expired = now + Duration::from_millis(2);
        for probe in [Probe::Authentication, Probe::Catalog] {
            let mut check = OperationalHealth::new(7, 1, now);
            check.probe = probe;
            check.child = Child::Waiting(9);
            for _ in 0..3 {
                assert!(!check.advance(expired, &mut |command| {
                    assert!(matches!(command, Command::TakeResult { request: 9 }));
                    Ok(Response::error("deadline_exceeded", "unconfirmed"))
                }));
                assert!(matches!(check.child, Child::Waiting(9)));
            }
        }
    }

    #[test]
    fn lost_admission_and_result_errors_remain_owned() {
        let now = Instant::now();
        let mut check = OperationalHealth::new(7, 5_000, now);
        check.advance(now, &mut |_| unreachable!());
        check.advance(now, &mut |_| Err("lost admission".into()));
        for code in ["permission_denied", "conflict", "unavailable"] {
            assert!(!check.quiesce(now, &mut |_| {
                Ok(Response::error(code, "temporary failure"))
            }));
            assert!(matches!(
                check.child,
                Child::Submit {
                    uncertain: true,
                    ..
                }
            ));
        }

        let mut check = OperationalHealth::new(8, 5_000, now);
        check.advance(now, &mut |_| unreachable!());
        check.advance(now, &mut |_| Ok(Response::Accepted { id: 9 }));
        assert!(!check.advance(now, &mut |_| Err("lost result".into())));
        assert!(matches!(check.child, Child::Waiting(9)));
        assert!(!check.advance(now, &mut |_| Ok(Response::Result {
            payload: json!({"ok":true,"verifier_available":true}),
        })));
        assert!(matches!(check.child, Child::Release { id: 9, .. }));
    }

    #[test]
    fn lookup_absence_only_retires_lost_admission_inside_retention_horizon() {
        let now = Instant::now();
        let mut check = OperationalHealth::new(7, 5_000, now);
        check.advance(now, &mut |_| unreachable!());
        check.advance(now, &mut |_| Err("lost admission".into()));
        let within = now + REQUEST_RETENTION - Duration::from_secs(1);
        assert!(!check.quiesce(within, &mut |command| {
            assert!(matches!(command, Command::LookupRequest { .. }));
            Ok(Response::error("not_found", "no matching request"))
        }));
        assert!(check.quiesce(within, &mut |command| {
            assert!(matches!(command, Command::Complete { .. }));
            Ok(Response::Ok)
        }));

        let mut check = OperationalHealth::new(8, 5_000, now);
        check.advance(now, &mut |_| unreachable!());
        check.advance(now, &mut |_| Err("lost admission".into()));
        let expired = now + REQUEST_RETENTION;
        for _ in 0..2 {
            assert!(!check.quiesce(expired, &mut |_| {
                Ok(Response::error("not_found", "no matching request"))
            }));
        }
        assert!(matches!(
            check.child,
            Child::Submit {
                uncertain: true,
                ..
            }
        ));
    }
}
