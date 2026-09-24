use crate::crypto;
use crate::product_settings::TokenLifetimes;
use crate::protocol::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub enum Change {
    Read(Value),
    WriteDocument {
        document: Document,
        response: Value,
        invalidate_sessions: bool,
    },
    WriteSessions {
        document: SessionDocument,
        response: Value,
    },
    WriteBoth {
        document: Document,
        sessions: SessionDocument,
        response: Value,
    },
}

#[derive(Clone)]
struct Session {
    credential_digest: String,
    subject: String,
    scopes: BTreeSet<String>,
    expires_at: u64,
    refresh_expires_at: u64,
    refresh_token_digest: String,
    remember_me: bool,
}

#[derive(Default)]
struct Attempts {
    start: u64,
    count: u32,
}

pub struct Engine {
    settings: Settings,
    sessions: BTreeMap<String, Session>,
    refresh_index: BTreeMap<String, String>,
    attempts: BTreeMap<String, Attempts>,
    global_attempts: Attempts,
    latest_time: u64,
}

impl Engine {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            sessions: BTreeMap::new(),
            refresh_index: BTreeMap::new(),
            attempts: BTreeMap::new(),
            global_attempts: Attempts::default(),
            latest_time: 0,
        }
    }

    fn observe_time(&mut self, now: u64) -> u64 {
        // Never extend a session inside this instance when wall time rolls back.
        self.latest_time = self.latest_time.max(now);
        self.sessions
            .retain(|_, session| session.refresh_expires_at >= self.latest_time);
        self.refresh_index
            .retain(|_, session_id| self.sessions.contains_key(session_id));
        self.latest_time
    }

    pub fn admit(
        &mut self,
        caller: &str,
        operation: &Operation,
        now: u64,
    ) -> Result<(), &'static str> {
        operation.validate()?;
        if operation.requires_administrator()
            && !self.settings.administrator_principals.contains(caller)
        {
            return Err("forbidden");
        }
        let now = self.observe_time(now);
        if matches!(operation, Operation::Login { .. }) {
            self.limit_login(caller, now)?;
        }
        Ok(())
    }

    fn limit_login(&mut self, caller: &str, now: u64) -> Result<(), &'static str> {
        self.attempts
            .retain(|_, attempts| now.saturating_sub(attempts.start) < 60);
        if now.saturating_sub(self.global_attempts.start) >= 60 {
            self.global_attempts = Attempts {
                start: now,
                count: 0,
            };
        }
        if self.global_attempts.count >= 20
            || (!self.attempts.contains_key(caller) && self.attempts.len() >= 128)
        {
            return Err("rate_limited");
        }
        let attempts = self.attempts.entry(caller.into()).or_insert(Attempts {
            start: now,
            count: 0,
        });
        if attempts.count >= 5 {
            return Err("rate_limited");
        }
        attempts.count += 1;
        self.global_attempts.count += 1;
        Ok(())
    }

    pub fn apply(
        &mut self,
        caller: &str,
        operation: Operation,
        document: Document,
        session_document: SessionDocument,
        now: u64,
    ) -> Result<Change, &'static str> {
        document.validate()?;
        session_document.validate()?;
        let now = self.observe_time(now);
        let credential_digest = document
            .credential
            .as_ref()
            .map(|credential| crypto::token_digest(&credential.password_hash));
        let scope_digest = Self::scope_digest(&self.settings.allowed_scopes);
        // Both sidecar bindings must match the current credential and exact
        // deterministic scope set before a session is trusted after restart.
        self.restore_sessions(
            &session_document,
            credential_digest.as_deref(),
            &scope_digest,
        );
        match operation {
            Operation::Status {} => Ok(Change::Read(json!({
                "ok": true,
                "verifier_available": document.credential.is_some(),
            }))),
            Operation::SettingsGet {} | Operation::SettingsUpdate { .. } => Err("invalid_request"),
            Operation::Setup { username, password } => Self::setup(document, username, password),
            Operation::ChangePassword { password } => Self::change_password(document, password),
            Operation::ListSessions { offset, limit } => self.list_sessions(offset, limit),
            Operation::RevokeSession { session_id } => {
                if self.remove_session(&session_id) {
                    self.write_sessions(json!({"ok":true}), &scope_digest)
                } else {
                    Ok(Change::Read(json!({"ok":true})))
                }
            }
            Operation::Login {
                username,
                password,
                remember_me,
            } => self.login(document, username, password, remember_me, now, scope_digest),
            Operation::Refresh { refresh_token } => self.refresh(refresh_token, now, scope_digest),
            Operation::Verify {
                bearer,
                required_scopes,
            } => self.verify(&document, bearer, required_scopes, now),
            Operation::Logout {
                bearer,
                refresh_token,
            } => {
                let removed = if let Some(bearer) = bearer {
                    self.remove_session(&crypto::token_digest(&bearer))
                } else if let Some(refresh_token) = refresh_token {
                    let refresh_digest = crypto::token_digest(&refresh_token);
                    self.refresh_index
                        .get(&refresh_digest)
                        .cloned()
                        .is_some_and(|session_id| self.remove_session(&session_id))
                } else {
                    false
                };
                if removed {
                    self.write_sessions(json!({"ok":true}), &scope_digest)
                } else {
                    Ok(Change::Read(json!({"ok":true})))
                }
            }
            Operation::IssueToken {
                scopes,
                ttl_seconds,
            } => self.issue_token(document, caller, scopes, ttl_seconds, now),
            Operation::RevokeToken { token_id } => Self::revoke_token(document, caller, token_id),
            Operation::ListTokens {} => Ok(Change::Read(
                json!({"ok":true,"tokens":document.tokens.iter().filter(|token| token.owner == caller).collect::<Vec<_>>()}),
            )),
        }
    }

    fn setup(
        mut document: Document,
        username: String,
        password: String,
    ) -> Result<Change, &'static str> {
        if document.credential.is_some() {
            return Err("already_initialized");
        }
        document.credential = Some(Credential {
            username,
            password_hash: crypto::hash_password(&password)?,
            legacy_salt: None,
            case_insensitive_username: false,
        });
        Self::write(document, json!({"ok":true}))
    }

    fn change_password(mut document: Document, password: String) -> Result<Change, &'static str> {
        let credential = document.credential.as_mut().ok_or("not_initialized")?;
        credential.password_hash = crypto::hash_password(&password)?;
        credential.legacy_salt = None;
        document.validate().map_err(|_| "resource_limit")?;
        Ok(Change::WriteDocument {
            document,
            response: json!({"ok":true}),
            invalidate_sessions: true,
        })
    }

    fn list_sessions(&self, offset: usize, limit: usize) -> Result<Change, &'static str> {
        let sessions: Vec<_> = self.sessions.iter().skip(offset).take(limit).map(|(session_id, session)| json!({"session_id":session_id,"subject":session.subject,"scopes":session.scopes,"expires_at":session.expires_at,"refresh_expires_at":session.refresh_expires_at,"remember_me":session.remember_me})).collect();
        let next_offset = offset
            .checked_add(sessions.len())
            .filter(|&next| next < self.sessions.len());
        Ok(Change::Read(
            json!({"ok":true,"sessions":sessions,"next_offset":next_offset}),
        ))
    }

    fn login(
        &mut self,
        mut document: Document,
        username: String,
        password: String,
        remember_me: bool,
        now: u64,
        scope_digest: String,
    ) -> Result<Change, &'static str> {
        let credential = document.credential.as_ref().ok_or("unauthorized")?;
        let valid_username = if credential.case_insensitive_username {
            username.eq_ignore_ascii_case(&credential.username)
        } else {
            username == credential.username
        };
        let valid_password = match &credential.legacy_salt {
            Some(salt) => {
                crypto::verify_legacy_password(&password, salt, &credential.password_hash)
            }
            None => crypto::verify_password(&password, &credential.password_hash),
        };
        if !valid_password || !valid_username {
            return Err("unauthorized");
        }
        if self.sessions.len() >= MAX_SESSIONS {
            return Err("session_limit");
        }
        let (bearer, digest) = crypto::issue_bearer()?;
        let (refresh_token, refresh_digest) = crypto::issue_bearer()?;
        let refresh_ttl = if remember_me {
            self.settings.remember_me_refresh_token_ttl_seconds
        } else {
            DEFAULT_REFRESH_TOKEN_TTL_SECONDS
        }
        .max(self.settings.session_token_ttl_seconds);
        let refresh_expires_at = now.checked_add(refresh_ttl).ok_or("clock_failure")?;
        let expires_at = now
            .checked_add(self.settings.session_token_ttl_seconds)
            .ok_or("clock_failure")?
            .min(refresh_expires_at);
        let scopes = self.settings.allowed_scopes.clone();
        let response = json!({"ok":true,"bearer":bearer,"refresh_token":refresh_token,"kind":"administrator_session","subject":username,"scopes":scopes,"expires_at":expires_at,"refresh_expires_at":refresh_expires_at,"remember_me":remember_me});
        let upgraded = credential.legacy_salt.is_some();
        if upgraded {
            let credential = document
                .credential
                .as_mut()
                .expect("credential checked above");
            credential.password_hash = crypto::hash_password(&password)?;
            credential.legacy_salt = None;
        }
        let credential = document
            .credential
            .as_ref()
            .expect("credential checked above");
        let credential_digest = crypto::token_digest(&credential.password_hash);
        self.sessions
            .retain(|_, session| session.credential_digest == credential_digest);
        self.refresh_index
            .retain(|_, session_id| self.sessions.contains_key(session_id));
        self.sessions.insert(
            digest.clone(),
            Session {
                credential_digest,
                subject: username,
                scopes,
                expires_at,
                refresh_expires_at,
                refresh_token_digest: refresh_digest.clone(),
                remember_me,
            },
        );
        self.refresh_index.insert(refresh_digest, digest);
        document.validate().map_err(|_| "resource_limit")?;
        let sessions = self.build_session_document(&scope_digest)?;
        if upgraded {
            Ok(Change::WriteBoth {
                document,
                sessions,
                response,
            })
        } else {
            Ok(Change::WriteSessions {
                document: sessions,
                response,
            })
        }
    }

    fn refresh(
        &mut self,
        refresh_token: String,
        now: u64,
        scope_digest: String,
    ) -> Result<Change, &'static str> {
        let refresh_digest = crypto::token_digest(&refresh_token);
        let session_id = self
            .refresh_index
            .get(&refresh_digest)
            .cloned()
            .ok_or("unauthorized")?;
        let Some(mut session) = self.sessions.get(&session_id).cloned() else {
            self.refresh_index.remove(&refresh_digest);
            return Err("unauthorized");
        };
        if session.refresh_token_digest != refresh_digest || session.refresh_expires_at < now {
            self.remove_session(&session_id);
            return Err("unauthorized");
        }

        // Mint the replacement pair before consuming the old refresh token. A
        // randomness or clock failure therefore leaves the current session usable.
        let (bearer, new_session_id) = crypto::issue_bearer()?;
        let (new_refresh_token, new_refresh_digest) = crypto::issue_bearer()?;
        let expires_at = now
            .checked_add(self.settings.session_token_ttl_seconds)
            .ok_or("clock_failure")?
            .min(session.refresh_expires_at);
        session.expires_at = expires_at;
        session.refresh_token_digest = new_refresh_digest.clone();

        self.remove_session(&session_id);
        self.sessions
            .insert(new_session_id.clone(), session.clone());
        self.refresh_index
            .insert(new_refresh_digest, new_session_id);

        let response = json!({
            "ok": true,
            "bearer": bearer,
            "refresh_token": new_refresh_token,
            "kind": "administrator_session",
            "subject": session.subject,
            "scopes": session.scopes,
            "expires_at": session.expires_at,
            "refresh_expires_at": session.refresh_expires_at,
            "remember_me": session.remember_me
        });
        self.write_sessions(response, &scope_digest)
    }

    fn remove_session(&mut self, session_id: &str) -> bool {
        if let Some(session) = self.sessions.remove(session_id) {
            self.refresh_index.remove(&session.refresh_token_digest);
            true
        } else {
            false
        }
    }

    fn restore_sessions(
        &mut self,
        document: &SessionDocument,
        credential_digest: Option<&str>,
        scope_digest: &str,
    ) {
        self.sessions.clear();
        self.refresh_index.clear();
        for record in &document.sessions {
            if Some(record.credential_digest.as_str()) != credential_digest
                || record.scope_digest != scope_digest
                || record.refresh_expires_at < self.latest_time
            {
                continue;
            }
            self.refresh_index.insert(
                record.refresh_token_digest.clone(),
                record.session_id.clone(),
            );
            self.sessions.insert(
                record.session_id.clone(),
                Session {
                    credential_digest: record.credential_digest.clone(),
                    subject: record.subject.clone(),
                    scopes: self.settings.allowed_scopes.clone(),
                    expires_at: record.expires_at,
                    refresh_expires_at: record.refresh_expires_at,
                    refresh_token_digest: record.refresh_token_digest.clone(),
                    remember_me: record.remember_me,
                },
            );
        }
    }

    fn build_session_document(&self, scope_digest: &str) -> Result<SessionDocument, &'static str> {
        let document = SessionDocument {
            version: SESSION_DOCUMENT_VERSION,
            sessions: self
                .sessions
                .iter()
                .map(|(session_id, session)| SessionRecord {
                    session_id: session_id.clone(),
                    credential_digest: session.credential_digest.clone(),
                    scope_digest: scope_digest.to_owned(),
                    subject: session.subject.clone(),
                    expires_at: session.expires_at,
                    refresh_expires_at: session.refresh_expires_at,
                    refresh_token_digest: session.refresh_token_digest.clone(),
                    remember_me: session.remember_me,
                })
                .collect(),
        };
        document.validate()?;
        Ok(document)
    }

    fn write_sessions(&self, response: Value, scope_digest: &str) -> Result<Change, &'static str> {
        Ok(Change::WriteSessions {
            document: self.build_session_document(scope_digest)?,
            response,
        })
    }

    fn scope_digest(scopes: &BTreeSet<String>) -> String {
        let canonical_scopes =
            serde_json::to_string(scopes).expect("serializing authentication scopes cannot fail");
        crypto::token_digest(&canonical_scopes)
    }

    fn verify(
        &self,
        document: &Document,
        bearer: String,
        required_scopes: BTreeSet<String>,
        now: u64,
    ) -> Result<Change, &'static str> {
        let digest = crypto::token_digest(&bearer);
        if let Some(session) = self.sessions.get(&digest) {
            if session.expires_at < now {
                return Err("unauthorized");
            }
            self.authorize_scopes(&session.scopes, &required_scopes)?;
            return Ok(Change::Read(
                json!({"ok":true,"kind":"administrator_session","subject":session.subject,"scopes":session.scopes,"expires_at":session.expires_at}),
            ));
        }
        let token = document
            .tokens
            .iter()
            .find(|token| token.token_id == digest && token.expires_at > now)
            .ok_or("unauthorized")?;
        self.authorize_scopes(&token.scopes, &required_scopes)?;
        Ok(Change::Read(
            json!({"ok":true,"kind":"api_token","subject":token.owner,"scopes":token.scopes,"expires_at":token.expires_at}),
        ))
    }

    fn issue_token(
        &self,
        mut document: Document,
        caller: &str,
        scopes: BTreeSet<String>,
        ttl_seconds: u64,
        now: u64,
    ) -> Result<Change, &'static str> {
        if document.credential.is_none() {
            return Err("not_initialized");
        }
        self.authorize_scopes(&scopes, &scopes)?;
        document.tokens.retain(|token| token.expires_at > now);
        if document.tokens.len() >= MAX_TOKENS {
            return Err("token_limit");
        }
        let (bearer, token_id) = crypto::issue_bearer()?;
        let expires_at = now.checked_add(ttl_seconds).ok_or("clock_failure")?;
        let response = json!({"ok":true,"bearer":bearer,"token_id":token_id,"scopes":scopes,"expires_at":expires_at});
        document.tokens.push(ApiToken {
            token_id,
            owner: caller.into(),
            scopes,
            issued_at: now,
            expires_at,
        });
        Self::write(document, response)
    }

    fn revoke_token(
        mut document: Document,
        caller: &str,
        token_id: String,
    ) -> Result<Change, &'static str> {
        let token = document
            .tokens
            .iter()
            .find(|token| token.token_id == token_id)
            .ok_or("token_not_found")?;
        if token.owner != caller {
            return Err("token_not_found");
        }
        document.tokens.retain(|token| token.token_id != token_id);
        Self::write(document, json!({"ok":true}))
    }

    pub fn clear_sessions(&mut self) {
        self.sessions.clear();
        self.refresh_index.clear();
    }

    pub fn set_token_lifetimes(&mut self, lifetimes: TokenLifetimes) {
        self.settings.session_token_ttl_seconds = lifetimes.session_token_ttl_seconds;
        self.settings.remember_me_refresh_token_ttl_seconds =
            lifetimes.remember_me_refresh_token_ttl_seconds;
    }

    fn authorize_scopes(
        &self,
        granted: &BTreeSet<String>,
        required: &BTreeSet<String>,
    ) -> Result<(), &'static str> {
        if !required.is_subset(granted) || !granted.is_subset(&self.settings.allowed_scopes) {
            return Err("unauthorized");
        }
        Ok(())
    }

    fn write(document: Document, response: Value) -> Result<Change, &'static str> {
        document.validate().map_err(|_| "resource_limit")?;
        Ok(Change::WriteDocument {
            document,
            response,
            invalidate_sessions: false,
        })
    }
}
