use crate::leases::LeaseRegistry;
use crate::protocol::{Action, DomainError, DomainResult, LeaseMode, Outcome, SessionState};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const MAX_SESSIONS: usize = 1024;
const MAX_CALLER_SESSIONS: usize = 256;
const MAX_SESSION_LEASES: usize = 128;
// JSON can escape one input byte into six output bytes. Together with lease
// metadata and the session key this budget keeps snapshots below 16 KiB.
const MAX_SESSION_RESOURCE_BYTES: usize = 1536;

#[derive(Serialize)]
struct Session {
    id: u64,
    #[serde(skip)]
    owner: String,
    key: String,
    state: SessionState,
    outcome: Option<Outcome>,
    leases: BTreeMap<String, LeaseMode>,
}

/// In-memory coordination only: an active record does not imply capture or streaming.
/// Stopped records remain until their owner explicitly forgets them.
#[derive(Default)]
pub struct Coordinator {
    sessions: BTreeMap<u64, Session>,
    leases: LeaseRegistry,
    next_id: u64,
    revision: u64,
}

impl Coordinator {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn handle(&mut self, caller: &str, payload: Value) -> Value {
        let action = serde_json::from_value(payload).map_err(|_| {
            DomainError::new(
                "invalid_request",
                "request does not match the session action schema",
            )
        });
        if let Ok(Action::List { offset, limit }) = action {
            return self.list(caller, offset, limit);
        }
        match action.and_then(|action| self.execute(caller, action)) {
            Ok(sessions) => json!({"ok":true,"revision":self.revision,"sessions":sessions}),
            Err(error) => {
                json!({"ok":false,"revision":self.revision,"code":error.code,"message":error.message})
            }
        }
    }

    fn execute(&mut self, caller: &str, action: Action) -> DomainResult<Value> {
        let id = match action {
            Action::Create { key } => self.create(caller, key)?,
            Action::Lookup { key } => self
                .sessions
                .values()
                .find(|session| session.owner == caller && session.key == key)
                .map(|session| session.id)
                .ok_or_else(|| {
                    DomainError::new(
                        "session_not_found",
                        "session is absent or belongs to another caller",
                    )
                })?,
            Action::List { .. } => unreachable!("list is handled without a session identifier"),
            action => {
                let id = action
                    .session_id()
                    .expect("session action has an identifier");
                self.authorize(caller, id)?;
                self.apply(id, action)?;
                id
            }
        };
        Ok(json!(
            self.sessions.get(&id).into_iter().collect::<Vec<_>>()
        ))
    }

    fn authorize(&self, caller: &str, id: u64) -> DomainResult<()> {
        match self.sessions.get(&id) {
            Some(session) if session.owner == caller => Ok(()),
            _ => Err(DomainError::new(
                "session_not_found",
                "session is absent or belongs to another caller",
            )),
        }
    }

    fn list(&self, caller: &str, offset: usize, limit: usize) -> Value {
        if !(1..=16).contains(&limit) {
            return json!({"ok":false,"revision":self.revision,"code":"invalid_limit","message":"list limit must be between 1 and 16"});
        }
        let mut owned = self
            .sessions
            .values()
            .filter(|session| session.owner == caller)
            .skip(offset);
        let sessions: Vec<_> = owned.by_ref().take(limit).map(Session::summary).collect();
        let next_offset = if owned.next().is_some() {
            offset.checked_add(sessions.len())
        } else {
            None
        };
        json!({"ok":true,"revision":self.revision,"sessions":sessions,"next_offset":next_offset})
    }

    fn create(&mut self, caller: &str, key: String) -> DomainResult<u64> {
        if key.is_empty() || key.len() > 128 {
            return Err(DomainError::new(
                "invalid_key",
                "key must contain 1 to 128 bytes",
            ));
        }
        if let Some(session) = self
            .sessions
            .values()
            .find(|session| session.owner == caller && session.key == key)
        {
            return Ok(session.id);
        }
        if self.sessions.len() >= MAX_SESSIONS
            || self
                .sessions
                .values()
                .filter(|session| session.owner == caller)
                .count()
                >= MAX_CALLER_SESSIONS
        {
            return Err(DomainError::new(
                "session_limit",
                "forget stopped sessions before creating more",
            ));
        }
        let id = self.next_id.checked_add(1).ok_or(DomainError::new(
            "id_exhausted",
            "session identifier space exhausted",
        ))?;
        self.next_id = id;
        self.sessions.insert(
            id,
            Session {
                id,
                owner: caller.to_owned(),
                key,
                state: SessionState::Preparing,
                outcome: None,
                leases: BTreeMap::new(),
            },
        );
        self.revision += 1;
        Ok(id)
    }

    fn apply(&mut self, id: u64, action: Action) -> DomainResult<()> {
        match action {
            Action::Activate { .. } => self.activate(id),
            Action::Stop { .. } => {
                self.stop(id);
                Ok(())
            }
            Action::FinishStop { .. } => self.finish_stop(id, Outcome::Completed),
            Action::Cancel { .. } => {
                self.stop(id);
                self.finish_stop(id, Outcome::Cancelled)
            }
            Action::Fail { .. } => {
                self.stop(id);
                self.finish_stop(id, Outcome::Failed)
            }
            Action::Acquire { resource, mode, .. } => self.acquire(id, resource, mode),
            Action::Release { resource, .. } => {
                self.release(id, &resource);
                Ok(())
            }
            Action::Forget { .. } => self.forget(id),
            Action::Snapshot { .. } => Ok(()),
            Action::Create { .. } | Action::Lookup { .. } | Action::List { .. } => {
                unreachable!("handled before session authorization")
            }
        }
    }

    fn activate(&mut self, id: u64) -> DomainResult<()> {
        let session = self.sessions.get_mut(&id).expect("authorized session");
        match session.state {
            SessionState::Preparing => {
                session.state = SessionState::Active;
                self.revision += 1;
                Ok(())
            }
            SessionState::Active => Ok(()),
            _ => Err(DomainError::new(
                "invalid_transition",
                "only a preparing session can become active",
            )),
        }
    }

    fn stop(&mut self, id: u64) {
        let session = self.sessions.get_mut(&id).expect("authorized session");
        if matches!(
            session.state,
            SessionState::Preparing | SessionState::Active
        ) {
            session.state = SessionState::Stopping;
            self.revision += 1;
        }
    }

    fn finish_stop(&mut self, id: u64, outcome: Outcome) -> DomainResult<()> {
        let session = self.sessions.get_mut(&id).expect("authorized session");
        match session.state {
            SessionState::Stopped => return Ok(()),
            SessionState::Stopping => {}
            _ => {
                return Err(DomainError::new(
                    "invalid_transition",
                    "session must be stopping before finishing stop",
                ));
            }
        }
        for resource in session.leases.keys() {
            self.leases.release(id, resource);
        }
        session.leases.clear();
        session.state = SessionState::Stopped;
        session.outcome = Some(outcome);
        self.revision += 1;
        Ok(())
    }

    fn acquire(&mut self, id: u64, resource: String, mode: LeaseMode) -> DomainResult<()> {
        let session = self.sessions.get_mut(&id).expect("authorized session");
        if !matches!(
            session.state,
            SessionState::Preparing | SessionState::Active
        ) {
            return Err(DomainError::new(
                "invalid_transition",
                "stopping and stopped sessions cannot acquire resources",
            ));
        }
        if !session.leases.contains_key(&resource) && session.leases.len() >= MAX_SESSION_LEASES {
            return Err(DomainError::new(
                "lease_limit",
                "session lease limit reached",
            ));
        }
        if !session.leases.contains_key(&resource)
            && session
                .leases
                .keys()
                .map(String::len)
                .sum::<usize>()
                .saturating_add(resource.len())
                > MAX_SESSION_RESOURCE_BYTES
        {
            return Err(DomainError::new(
                "lease_limit",
                "session resource identity byte budget exceeded",
            ));
        }
        if self.leases.acquire(id, &resource, mode)? {
            session.leases.insert(resource, mode);
            self.revision += 1;
        }
        Ok(())
    }

    fn release(&mut self, id: u64, resource: &str) {
        if self.leases.release(id, resource) {
            self.sessions
                .get_mut(&id)
                .expect("authorized session")
                .leases
                .remove(resource);
            self.revision += 1;
        }
    }

    fn forget(&mut self, id: u64) -> DomainResult<()> {
        if self.sessions[&id].state != SessionState::Stopped {
            return Err(DomainError::new(
                "invalid_transition",
                "only stopped sessions can be forgotten",
            ));
        }
        self.sessions.remove(&id);
        self.revision += 1;
        Ok(())
    }
}

impl Session {
    // Summaries keep even a full caller inventory below the ABI message limit.
    // Lease details are available in the individual session snapshot.
    fn summary(&self) -> Value {
        json!({
            "id": self.id,
            "key": self.key,
            "state": self.state,
            "outcome": self.outcome,
            "lease_count": self.leases.len(),
        })
    }
}

impl Action {
    fn session_id(&self) -> Option<u64> {
        match self {
            Self::Activate { session_id }
            | Self::Stop { session_id }
            | Self::FinishStop { session_id }
            | Self::Cancel { session_id }
            | Self::Fail { session_id }
            | Self::Snapshot { session_id }
            | Self::Forget { session_id }
            | Self::Acquire { session_id, .. }
            | Self::Release { session_id, .. } => Some(*session_id),
            Self::Create { .. } | Self::Lookup { .. } | Self::List { .. } => None,
        }
    }
}
