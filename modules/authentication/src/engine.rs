use crate::accounts::{self, Account, Role};
use crate::crypto;
use crate::legacy_tokens::LegacyApiTokenStore;
use crate::product_settings::TokenLifetimes;
use crate::protocol::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use subtle::ConstantTimeEq;

// Reserved owner namespace distinguishes browser-issued credentials without
// changing the persisted token schema used by older package versions.
const BROWSER_TOKEN_OWNER: &str = "amaterasu.browser-token";

pub enum Change {
    Read(Value),
    ReadProductSettings,
    UpdateProductSettings {
        session_token_ttl_seconds: u64,
        remember_me_refresh_token_ttl_seconds: u64,
        expected_revision: u64,
    },
    WriteDocument {
        document: Document,
        response: Value,
        invalidate_sessions: bool,
    },
    WriteSessions {
        document: SessionDocument,
        response: Value,
    },
    WriteLegacyApiTokens {
        document: LegacyApiTokenStore,
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
    role: Role,
    policy_generation: u64,
    ownership_context_id: String,
    subject: String,
    scopes: BTreeSet<String>,
    expires_at: u64,
    refresh_expires_at: u64,
    refresh_token_digest: String,
    remember_me: bool,
}

struct ManagementBudget {
    tokens: u32,
    last_refill: Option<u64>,
}

impl Default for ManagementBudget {
    fn default() -> Self {
        Self {
            tokens: 20,
            last_refill: None,
        }
    }
}

impl ManagementBudget {
    fn admit(&mut self, now: u64) -> Result<(), &'static str> {
        // This is a process-wide backstop, not a peer limiter. The management
        // service applies its own limits using the actual socket address. A
        // short burst cannot impose a full-minute lockout on other clients.
        if let Some(last) = self.last_refill {
            let elapsed = now.saturating_sub(last).min(10);
            self.tokens = self.tokens.saturating_add((elapsed as u32) * 2).min(20);
        }
        self.last_refill = Some(now);
        if self.tokens == 0 {
            return Err("rate_limited");
        }
        self.tokens -= 1;
        Ok(())
    }
}

#[derive(Default)]
struct Attempts {
    start: u64,
    count: u32,
}

pub struct Engine {
    settings: Settings,
    /// The built-in scopes plus the host policy's. An administrator holds all
    /// of them, and no session or token may carry a scope outside this set.
    administrator_scopes: BTreeSet<String>,
    sessions: BTreeMap<String, Session>,
    refresh_index: BTreeMap<String, String>,
    attempts: BTreeMap<String, Attempts>,
    global_attempts: Attempts,
    management_budget: ManagementBudget,
    latest_time: u64,
}

impl Engine {
    pub fn new(settings: Settings) -> Self {
        Self {
            administrator_scopes: accounts::administrator_scopes(&settings.allowed_scopes),
            settings,
            sessions: BTreeMap::new(),
            refresh_index: BTreeMap::new(),
            attempts: BTreeMap::new(),
            global_attempts: Attempts::default(),
            management_budget: ManagementBudget::default(),
            latest_time: 0,
        }
    }

    fn observe_time(&mut self, now: u64) -> u64 {
        // Never extend a session inside this instance when wall time rolls back.
        self.latest_time = self.latest_time.max(now);
        self.sessions
            .retain(|_, session| session.refresh_expires_at > self.latest_time);
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
        if matches!(operation, Operation::GenerateEnrollmentCode {})
            && caller != "amaterasu.console"
        {
            return Err("forbidden");
        }
        if matches!(operation, Operation::EnrollFirstAdmin { .. })
            && caller != "amaterasu.management"
        {
            return Err("forbidden");
        }
        if matches!(
            operation,
            Operation::AccountTokensList { .. }
                | Operation::AccountTokensIssue { .. }
                | Operation::AccountTokensRevoke { .. }
        ) && caller != "amaterasu.management"
        {
            return Err("forbidden");
        }
        if matches!(
            operation,
            Operation::AccountSessionsList { .. } | Operation::AccountSessionsRevoke { .. }
        ) && caller != "amaterasu.management"
        {
            return Err("forbidden");
        }
        if operation.requires_administrator()
            && !self.settings.administrator_principals.contains(caller)
        {
            return Err("forbidden");
        }
        if matches!(
            operation,
            Operation::Verify {
                legacy_request: Some(_),
                ..
            }
        ) && !self.settings.administrator_principals.contains(caller)
            && !self.settings.legacy_verifier_principals.contains(caller)
        {
            return Err("forbidden");
        }
        let now = self.observe_time(now);
        if matches!(
            operation,
            Operation::Login { .. } | Operation::EnrollFirstAdmin { .. }
        ) {
            self.limit_login(caller, now)?;
        }
        Ok(())
    }

    fn limit_login(&mut self, caller: &str, now: u64) -> Result<(), &'static str> {
        if caller == "amaterasu.management" {
            return self.management_budget.admit(now);
        }
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
        mut legacy_api_tokens: LegacyApiTokenStore,
        now: u64,
    ) -> Result<Change, &'static str> {
        document.validate()?;
        session_document.validate()?;
        legacy_api_tokens.validate()?;
        let now = self.observe_time(now);
        let scope_digest = self.policy_digest();
        // Sidecar bindings must match the current credential, exact scope set,
        // and monotonic policy generation before a session is trusted.
        self.restore_sessions(&session_document, &document, &scope_digest);
        match operation {
            Operation::GenerateEnrollmentCode {} => {
                if document.credential.is_some() || !document.accounts.is_empty() {
                    return Err("already_initialized");
                }
                let (code, code_digest) = crypto::issue_bearer()?;
                let expires_at = now.checked_add(600).ok_or("clock_failure")?;
                let mut document = document;
                document.enrollment = Some(Enrollment {
                    code_digest,
                    expires_at,
                });
                Self::write(
                    document,
                    json!({"ok":true,"code":code,"expires_at":expires_at}),
                )
            }
            Operation::EnrollFirstAdmin {
                code,
                username,
                password,
            } => {
                if document.credential.is_some() || !document.accounts.is_empty() {
                    return Err("already_initialized");
                }
                let enrollment = document.enrollment.as_ref().ok_or("unauthorized")?;
                if enrollment.expires_at <= now
                    || !bool::from(
                        enrollment
                            .code_digest
                            .as_bytes()
                            .ct_eq(crypto::token_digest(&code).as_bytes()),
                    )
                {
                    return Err("unauthorized");
                }
                let mut document = document;
                document.enrollment = None;
                accounts::add_account(
                    &mut document.accounts,
                    None,
                    Account {
                        username,
                        password_hash: crypto::hash_password(&password)?,
                        authorization_id: String::new(),
                        role: Role::Administrator,
                        enabled: true,
                    },
                )?;
                Self::write(document, json!({"ok":true}))
            }
            Operation::ListAccounts { bearer } => {
                self.require_admin_session(&bearer, now)?;
                let mut accounts = document
                    .accounts
                    .iter()
                    .map(|a| json!({"username":a.username,"role":a.role,"enabled":a.enabled}))
                    .collect::<Vec<_>>();
                if let Some(legacy) = &document.credential {
                    accounts.push(json!({"username":legacy.username,"role":Role::Administrator,"enabled":true}));
                }
                Ok(Change::Read(json!({"ok":true,"accounts":accounts})))
            }
            Operation::CreateAccount {
                bearer,
                username,
                password,
                role,
            } => {
                self.require_admin_session(&bearer, now)?;
                let mut document = document;
                accounts::add_account(
                    &mut document.accounts,
                    document.credential.as_ref(),
                    Account {
                        username,
                        password_hash: crypto::hash_password(&password)?,
                        authorization_id: String::new(),
                        role,
                        enabled: true,
                    },
                )?;
                Self::write(document, json!({"ok":true}))
            }
            Operation::UpdateAccount {
                bearer,
                username,
                role,
                enabled,
            } => {
                self.require_admin_session(&bearer, now)?;
                let mut document = document;
                Self::migrate_legacy(&mut document)?;
                accounts::update_account(&mut document.accounts, None, &username, role, enabled)?;
                Self::write(document, json!({"ok":true}))
            }
            Operation::DeleteAccount { bearer, username } => {
                self.require_admin_session(&bearer, now)?;
                let mut document = document;
                Self::migrate_legacy(&mut document)?;
                accounts::delete_account(&mut document.accounts, None, &username)?;
                Self::write(document, json!({"ok":true}))
            }
            Operation::SetAccountPassword {
                bearer,
                username,
                password,
            } => {
                self.require_admin_session(&bearer, now)?;
                let mut document = document;
                Self::migrate_legacy(&mut document)?;
                let account = document
                    .accounts
                    .iter_mut()
                    .find(|a| a.username.eq_ignore_ascii_case(&username))
                    .ok_or("account_not_found")?;
                accounts::rotate_authorization(account)?;
                account.password_hash = crypto::hash_password(&password)?;
                Self::write(document, json!({"ok":true}))
            }
            Operation::ChangeOwnPassword {
                bearer,
                current_password,
                new_password,
            } => {
                let subject = self.active_session(&bearer, now)?.subject.clone();
                let mut document = document;
                Self::migrate_legacy(&mut document)?;
                let account = document
                    .accounts
                    .iter_mut()
                    .find(|a| a.username == subject)
                    .ok_or("unauthorized")?;
                if !crypto::verify_password(&current_password, &account.password_hash) {
                    return Err("unauthorized");
                }
                accounts::rotate_authorization(account)?;
                account.password_hash = crypto::hash_password(&new_password)?;
                Self::write(document, json!({"ok":true}))
            }
            Operation::Status {} => Ok(Change::Read(json!({
                "ok": true,
                "verifier_available": document.credential.is_some() || !document.accounts.is_empty(),
            }))),
            Operation::AccountSettingsGet { bearer } => {
                self.require_admin_session(&bearer, now)?;
                Ok(Change::ReadProductSettings)
            }
            Operation::AccountSettingsUpdate {
                bearer,
                session_token_ttl_seconds,
                remember_me_refresh_token_ttl_seconds,
                expected_revision,
            } => {
                self.require_admin_session(&bearer, now)?;
                Ok(Change::UpdateProductSettings {
                    session_token_ttl_seconds,
                    remember_me_refresh_token_ttl_seconds,
                    expected_revision,
                })
            }
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
            Operation::AccountSessionsList { bearer } => self.account_sessions_list(&bearer, now),
            Operation::AccountSessionsRevoke { bearer, session_id } => {
                self.account_session_revoke(&bearer, &session_id, now, &scope_digest)
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
                legacy_request,
            } => self.verify(
                &document,
                &mut legacy_api_tokens,
                bearer,
                required_scopes,
                legacy_request,
                now,
            ),
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
            Operation::RevokeToken { token_id } => {
                Self::revoke_token(document, legacy_api_tokens, caller, token_id)
            }
            Operation::AccountTokensList { bearer } => {
                self.require_token_management_session(&bearer, now)?;
                Ok(Change::Read(
                    json!({"ok":true,"tokens": document.tokens.iter()
                    .filter(|token| token.owner == BROWSER_TOKEN_OWNER && self.token_is_active(token, now))
                    .collect::<Vec<_>>() }),
                ))
            }
            Operation::AccountTokensIssue {
                bearer,
                scopes,
                ttl_seconds,
            } => {
                self.require_token_management_session(&bearer, now)?;
                let session = self.active_session(&bearer, now)?;
                self.authorize_scopes(&session.scopes, &scopes)?;
                self.issue_token(document, BROWSER_TOKEN_OWNER, scopes, ttl_seconds, now)
            }
            Operation::AccountTokensRevoke { bearer, token_id } => {
                self.require_token_management_session(&bearer, now)?;
                let mut document = document;
                if !document
                    .tokens
                    .iter()
                    .any(|token| token.owner == BROWSER_TOKEN_OWNER && token.token_id == token_id)
                {
                    return Err("token_not_found");
                }
                document.tokens.retain(|token| {
                    token.owner != BROWSER_TOKEN_OWNER || token.token_id != token_id
                });
                Self::write(document, json!({"ok":true}))
            }
            Operation::ListTokens {} => Ok(Change::Read(json!({
                "ok":true,
                "tokens":document.tokens.iter().filter(|token| token.owner == caller && self.token_is_active(token, now)).collect::<Vec<_>>(),
                "legacy_tokens":legacy_api_tokens.tokens.iter().map(|token| json!({
                    "token_id":token.token_id,
                    "owner":token.owner,
                    "created_at":token.created_at,
                    "expires_at":null
                })).collect::<Vec<_>>()
            }))),
        }
    }

    /// Remove records that cannot be trusted under the current credential,
    /// authorization policy, or time before a request uses the sidecar.
    pub fn prune_sessions(
        &self,
        credential_document: &Document,
        mut session_document: SessionDocument,
        now: u64,
    ) -> (SessionDocument, bool) {
        let scope_digest = self.policy_digest();
        let original_len = session_document.sessions.len();
        let original_version = session_document.version;
        session_document.sessions.retain_mut(|session| {
            let eligible = Self::identity(credential_document, &session.subject)
                .is_some_and(|(digest, _)| session.credential_digest == digest)
                && session.scope_digest == scope_digest
                && session.policy_generation == Some(self.settings.authorization_policy_generation)
                && session.refresh_expires_at > now.max(self.latest_time);
            if eligible && original_version < SESSION_DOCUMENT_VERSION {
                session.ownership_context_id = Some(session.session_id.clone());
            }
            eligible
        });
        session_document.version = SESSION_DOCUMENT_VERSION;
        let removed = session_document.sessions.len() != original_len
            || original_version != SESSION_DOCUMENT_VERSION;
        (session_document, removed)
    }

    fn identity(document: &Document, subject: &str) -> Option<(String, Role)> {
        if let Some(credential) = document
            .credential
            .as_ref()
            .filter(|c| c.username == subject)
        {
            return Some((
                crypto::token_digest(&credential.password_hash),
                Role::Administrator,
            ));
        }
        let account = document
            .accounts
            .iter()
            .find(|a| a.username == subject && a.enabled)?;
        Some((accounts::credential_digest(account), account.role))
    }

    fn active_session(&self, bearer: &str, now: u64) -> Result<&Session, &'static str> {
        self.sessions
            .get(&crypto::token_digest(bearer))
            .filter(|s| s.expires_at > now)
            .ok_or("unauthorized")
    }

    fn require_token_management_session(&self, bearer: &str, now: u64) -> Result<(), &'static str> {
        self.require_admin_session(bearer, now)?;
        if !self
            .active_session(bearer, now)?
            .scopes
            .contains("tokens.manage")
        {
            return Err("forbidden");
        }
        Ok(())
    }

    fn require_admin_session(&self, bearer: &str, now: u64) -> Result<(), &'static str> {
        if self.active_session(bearer, now)?.role != Role::Administrator {
            return Err("forbidden");
        }
        Ok(())
    }

    // Migrating the credential is one CAS-protected document write. Session
    // bindings change, so interrupted sidecar updates cannot preserve access.
    fn migrate_legacy(document: &mut Document) -> Result<(), &'static str> {
        if let Some(credential) = document.credential.take() {
            if credential.legacy_salt.is_some() {
                return Err("login_required");
            }
            accounts::add_account(
                &mut document.accounts,
                None,
                Account {
                    username: credential.username,
                    password_hash: credential.password_hash,
                    authorization_id: String::new(),
                    role: Role::Administrator,
                    enabled: true,
                },
            )?;
        }
        Ok(())
    }

    fn setup(
        mut document: Document,
        username: String,
        password: String,
    ) -> Result<Change, &'static str> {
        if document.credential.is_some() || !document.accounts.is_empty() {
            return Err("already_initialized");
        }
        document.enrollment = None;
        document.credential = Some(Credential {
            username,
            password_hash: crypto::hash_password(&password)?,
            legacy_salt: None,
            case_insensitive_username: false,
        });
        Self::write(document, json!({"ok":true}))
    }

    fn change_password(mut document: Document, password: String) -> Result<Change, &'static str> {
        if let Some(credential) = document.credential.as_mut() {
            credential.password_hash = crypto::hash_password(&password)?;
            credential.legacy_salt = None;
        } else {
            let mut admins = document
                .accounts
                .iter_mut()
                .filter(|account| account.enabled && account.role == Role::Administrator);
            let account = admins.next().ok_or("not_initialized")?;
            if admins.next().is_some() {
                return Err("ambiguous_account");
            }
            accounts::rotate_authorization(account)?;
            account.password_hash = crypto::hash_password(&password)?;
        }
        document.validate().map_err(|_| "resource_limit")?;
        Ok(Change::WriteDocument {
            document,
            response: json!({"ok":true}),
            invalidate_sessions: true,
        })
    }

    fn account_sessions_list(&self, bearer: &str, now: u64) -> Result<Change, &'static str> {
        let current = self.require_session_management(bearer, now)?;
        let sessions = self
            .sessions
            .values()
            .map(|session| {
                json!({
                    "session_id": session.ownership_context_id,
                    "subject": session.subject,
                    "expires_at": session.expires_at,
                    "refresh_expires_at": session.refresh_expires_at,
                    "remember_me": session.remember_me,
                    "current": session.ownership_context_id == current.ownership_context_id,
                })
            })
            .collect::<Vec<_>>();
        Ok(Change::Read(json!({"ok":true,"sessions":sessions})))
    }

    fn account_session_revoke(
        &mut self,
        bearer: &str,
        target: &str,
        now: u64,
        scope_digest: &str,
    ) -> Result<Change, &'static str> {
        let current = self.require_session_management(bearer, now)?;
        if current.ownership_context_id == target {
            return Err("current_session");
        }
        let access_id = self
            .sessions
            .iter()
            .find_map(|(id, session)| (session.ownership_context_id == target).then(|| id.clone()));
        if let Some(access_id) = access_id {
            self.remove_session(&access_id);
            self.write_sessions(json!({"ok":true,"revoked":true}), scope_digest)
        } else {
            Ok(Change::Read(json!({"ok":true,"revoked":false})))
        }
    }

    fn require_session_management(&self, bearer: &str, now: u64) -> Result<&Session, &'static str> {
        self.require_admin_session(bearer, now)?;
        let session = self.active_session(bearer, now)?;
        if !session.scopes.contains("configuration.write") {
            return Err("forbidden");
        }
        Ok(session)
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
        let legacy = document.credential.as_ref().filter(|credential| {
            if credential.case_insensitive_username {
                username.eq_ignore_ascii_case(&credential.username)
            } else {
                username == credential.username
            }
        });
        let (subject, role, migrate, upgrade_hash) = if let Some(credential) = legacy {
            let valid = match &credential.legacy_salt {
                Some(salt) => {
                    crypto::verify_legacy_password(&password, salt, &credential.password_hash)
                }
                None => crypto::verify_password(&password, &credential.password_hash),
            };
            if !valid {
                return Err("unauthorized");
            }
            (
                credential.username.clone(),
                Role::Administrator,
                true,
                credential.legacy_salt.is_some(),
            )
        } else {
            let account =
                accounts::find_account(&document.accounts, &username).ok_or("unauthorized")?;
            if !account.enabled || !crypto::verify_password(&password, &account.password_hash) {
                return Err("unauthorized");
            }
            (account.username.clone(), account.role, false, false)
        };
        let (bearer, digest) = crypto::issue_bearer()?;
        let (_, ownership_context_id) = crypto::issue_bearer()?;
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
        let scopes = self.role_scopes(role);
        let response = json!({"ok":true,"bearer":bearer,"refresh_token":refresh_token,"kind":"administrator_session","subject":subject,"role":role,"scopes":scopes,"expires_at":expires_at,"refresh_expires_at":refresh_expires_at,"remember_me":remember_me});
        if upgrade_hash {
            let credential = document
                .credential
                .as_mut()
                .expect("credential checked above");
            credential.password_hash = crypto::hash_password(&password)?;
            credential.legacy_salt = None;
        }
        if migrate {
            Self::migrate_legacy(&mut document)?;
            self.sessions
                .retain(|_, session| session.subject != subject);
            self.refresh_index
                .retain(|_, id| self.sessions.contains_key(id));
        }
        let (credential_digest, _) = Self::identity(&document, &subject).ok_or("unauthorized")?;
        document.validate().map_err(|_| "resource_limit")?;
        if self.sessions.len() >= MAX_SESSIONS {
            // A client can lose every bearer during a reload while its
            // remembered sessions remain durable. Reclaim the session with
            // the earliest refresh deadline after password proof and token
            // generation; the digest provides a stable tie break.
            let earliest_expiring_session_id = self
                .sessions
                .iter()
                .min_by_key(|(session_id, session)| (session.refresh_expires_at, *session_id))
                .map(|(session_id, _)| session_id.clone())
                .expect("a full session set has a reclaimable session");
            self.remove_session(&earliest_expiring_session_id);
        }
        self.sessions.insert(
            digest.clone(),
            Session {
                credential_digest,
                role,
                policy_generation: self.settings.authorization_policy_generation,
                ownership_context_id,
                subject,
                scopes,
                expires_at,
                refresh_expires_at,
                refresh_token_digest: refresh_digest.clone(),
                remember_me,
            },
        );
        self.refresh_index.insert(refresh_digest, digest);
        let sessions = self.build_session_document(&scope_digest)?;
        if migrate {
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
        if session.refresh_token_digest != refresh_digest || session.refresh_expires_at <= now {
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
            "role": session.role,
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
        credentials: &Document,
        scope_digest: &str,
    ) {
        self.sessions.clear();
        self.refresh_index.clear();
        for record in &document.sessions {
            let Some((credential_digest, role)) = Self::identity(credentials, &record.subject)
            else {
                continue;
            };
            if record.credential_digest != credential_digest
                || record.scope_digest != scope_digest
                || record.policy_generation != Some(self.settings.authorization_policy_generation)
                || record.refresh_expires_at <= self.latest_time
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
                    role,
                    policy_generation: record
                        .policy_generation
                        .expect("session generation was validated before restoration"),
                    ownership_context_id: record
                        .ownership_context_id
                        .clone()
                        .unwrap_or_else(|| record.session_id.clone()),
                    subject: record.subject.clone(),
                    scopes: self.role_scopes(role),
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
                    policy_generation: Some(session.policy_generation),
                    ownership_context_id: Some(session.ownership_context_id.clone()),
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

    fn role_scopes(&self, role: Role) -> BTreeSet<String> {
        let mut scopes = accounts::role_scopes(role, &self.settings.allowed_scopes);
        for (scope, minimum) in &self.settings.scope_minimum_roles {
            if role == Role::Administrator
                || *minimum == Role::Viewer
                || (role == Role::Operator && *minimum == Role::Operator)
            {
                scopes.insert(scope.clone());
            }
        }
        scopes
    }

    fn policy_digest(&self) -> String {
        if self.settings.scope_minimum_roles.is_empty() {
            return Self::scope_digest(&self.settings.allowed_scopes);
        }
        let canonical = serde_json::to_string(&(
            &self.settings.allowed_scopes,
            &self.settings.scope_minimum_roles,
        ))
        .expect("serializing authentication policy cannot fail");
        crypto::token_digest(&canonical)
    }

    fn scope_digest(scopes: &BTreeSet<String>) -> String {
        let canonical_scopes =
            serde_json::to_string(scopes).expect("serializing authentication scopes cannot fail");
        crypto::token_digest(&canonical_scopes)
    }

    fn verify(
        &self,
        document: &Document,
        legacy_api_tokens: &mut LegacyApiTokenStore,
        bearer: String,
        required_scopes: BTreeSet<String>,
        legacy_request: Option<LegacyRequestContext>,
        now: u64,
    ) -> Result<Change, &'static str> {
        let digest = crypto::token_digest(&bearer);
        if let Some(session) = self.sessions.get(&digest) {
            if session.expires_at <= now {
                return Err("unauthorized");
            }
            self.authorize_scopes(&session.scopes, &required_scopes)?;
            return Ok(Change::Read(
                json!({"ok":true,"kind":"administrator_session","subject":session.subject,"role":session.role,"scopes":session.scopes,"expires_at":session.expires_at,"auth_context_id":session.ownership_context_id}),
            ));
        }
        if let Some(token) = document
            .tokens
            .iter()
            .find(|token| token.token_id == digest && self.token_is_active(token, now))
        {
            self.authorize_scopes(&token.scopes, &required_scopes)?;
            return Ok(Change::Read(json!({
                "ok":true,
                "kind":"api_token",
                "subject":token.owner,
                "scopes":token.scopes,
                "expires_at":token.expires_at,
                "auth_context_id":token.token_id
            })));
        }

        let Some(legacy_request) = legacy_request else {
            return Err("unauthorized");
        };
        if required_scopes.is_empty() || !required_scopes.is_subset(&self.settings.allowed_scopes) {
            return Err("unauthorized");
        }
        let verification = legacy_api_tokens
            .verify_route(&bearer, &legacy_request.path, &legacy_request.method)?
            .ok_or("unauthorized")?;
        let response = json!({
            "ok":true,
            "kind":"api_token",
            "subject":verification.owner,
            "scopes":required_scopes,
            "expires_at":null,
            "auth_context_id":verification.token_id
        });
        if let Some(document) = verification.upgraded_store {
            Ok(Change::WriteLegacyApiTokens { document, response })
        } else {
            Ok(Change::Read(response))
        }
    }

    fn issue_token(
        &self,
        mut document: Document,
        owner: &str,
        scopes: BTreeSet<String>,
        ttl_seconds: u64,
        now: u64,
    ) -> Result<Change, &'static str> {
        if document.credential.is_none() && document.accounts.is_empty() {
            return Err("not_initialized");
        }
        self.authorize_scopes(&scopes, &scopes)?;
        document
            .tokens
            .retain(|token| self.token_is_active(token, now));
        if document.tokens.len() >= MAX_TOKENS {
            return Err("token_limit");
        }
        let (bearer, token_id) = crypto::issue_bearer()?;
        let expires_at = now.checked_add(ttl_seconds).ok_or("clock_failure")?;
        let response = json!({"ok":true,"bearer":bearer,"token_id":token_id,"scopes":scopes,"expires_at":expires_at});
        document.tokens.push(ApiToken {
            token_id,
            owner: owner.into(),
            policy_generation: Some(self.settings.authorization_policy_generation),
            scopes,
            issued_at: now,
            expires_at,
        });
        Self::write(document, response)
    }

    fn revoke_token(
        mut document: Document,
        mut legacy_api_tokens: LegacyApiTokenStore,
        caller: &str,
        token_id: String,
    ) -> Result<Change, &'static str> {
        if let Some(token) = document
            .tokens
            .iter()
            .find(|token| token.token_id == token_id)
        {
            if token.owner != caller {
                return Err("token_not_found");
            }
            document.tokens.retain(|token| token.token_id != token_id);
            return Self::write(document, json!({"ok":true}));
        }

        if !legacy_api_tokens.revoke(&token_id) {
            return Err("token_not_found");
        }
        legacy_api_tokens.validate().map_err(|_| "resource_limit")?;
        Ok(Change::WriteLegacyApiTokens {
            document: legacy_api_tokens,
            response: json!({"ok":true}),
        })
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
        if !required.is_subset(granted) || !granted.is_subset(&self.administrator_scopes) {
            return Err("unauthorized");
        }
        Ok(())
    }

    fn token_is_active(&self, token: &ApiToken, now: u64) -> bool {
        token.expires_at > now
            && token.policy_generation == Some(self.settings.authorization_policy_generation)
            && (token.owner == BROWSER_TOKEN_OWNER
                || self
                    .settings
                    .administrator_principals
                    .contains(&token.owner))
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
