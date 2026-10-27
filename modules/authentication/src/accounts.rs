//! Account state and least-privilege role policy.
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::protocol::Credential;

pub const MAX_ACCOUNTS: usize = 32;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Administrator,
    Operator,
    Viewer,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub username: String,
    pub password_hash: String,
    pub role: Role,
    pub enabled: bool,
    /// Random generation rotated whenever authorization changes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub authorization_id: String,
}

/// Matches the existing credential identifier grammar. ASCII case folding
/// prevents ambiguous names, including collisions with migrated credentials.
pub fn valid_username(username: &str) -> bool {
    !username.is_empty()
        && username.len() <= 64
        && username
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:/".contains(&b))
}

pub fn validate_accounts(
    accounts: &[Account],
    legacy: Option<&Credential>,
) -> Result<(), &'static str> {
    if accounts.len() + usize::from(legacy.is_some()) > MAX_ACCOUNTS {
        return Err("invalid_state");
    }
    let mut names = BTreeSet::new();
    if let Some(credential) = legacy {
        names.insert(credential.username.to_ascii_lowercase());
    }
    for account in accounts {
        if !valid_username(&account.username)
            || !names.insert(account.username.to_ascii_lowercase())
            || !crate::crypto::valid_password_hash(&account.password_hash)
            || (!account.authorization_id.is_empty()
                && !crate::protocol::valid_digest(&account.authorization_id))
        {
            return Err("invalid_state");
        }
    }
    if legacy.is_none()
        && !accounts.is_empty()
        && !accounts
            .iter()
            .any(|a| a.enabled && a.role == Role::Administrator)
    {
        return Err("invalid_state");
    }
    Ok(())
}

pub fn find_account<'a>(accounts: &'a [Account], username: &str) -> Option<&'a Account> {
    accounts
        .iter()
        .find(|account| account.username.eq_ignore_ascii_case(username))
}

/// Role grants are a fixed allowlist, intersected with the host policy. Newly
/// introduced scopes therefore remain administrator-only until explicitly reviewed.
pub fn role_scopes(role: Role, allowed: &BTreeSet<String>) -> BTreeSet<String> {
    const VIEWER: &[&str] = &[
        "account.self",
        "application.read",
        "capture.read",
        "catalog.read",
        "configuration.read",
        "diagnostics.read",
        "display.read",
        "lutris.read",
        "media.read",
        "session.read",
        "steam.read",
    ];
    const OPERATOR: &[&str] = &[
        "application.launch",
        "application.terminate",
        "session.write",
        "steam.refresh",
    ];
    allowed
        .iter()
        .filter(|scope| match role {
            Role::Administrator => true,
            Role::Operator => {
                VIEWER.contains(&scope.as_str()) || OPERATOR.contains(&scope.as_str())
            }
            Role::Viewer => VIEWER.contains(&scope.as_str()),
        })
        .cloned()
        .collect()
}

/// Binding changes on credential or authorization changes, revoking sessions
/// even if a state write commits but subsequent session pruning does not.
pub fn credential_digest(account: &Account) -> String {
    let binding = serde_json::to_string(&(
        "account-v1",
        &account.username,
        &account.password_hash,
        account.role,
        account.enabled,
        &account.authorization_id,
    ))
    .expect("account binding serialization cannot fail");
    crate::crypto::token_digest(&binding)
}

/// Invalidate all earlier sessions, including after settings later revert.
pub fn rotate_authorization(account: &mut Account) -> Result<(), &'static str> {
    let (_, generation) = crate::crypto::issue_bearer()?;
    account.authorization_id = generation;
    Ok(())
}

pub fn add_account(
    accounts: &mut Vec<Account>,
    legacy: Option<&Credential>,
    mut account: Account,
) -> Result<(), &'static str> {
    if accounts.len() + usize::from(legacy.is_some()) >= MAX_ACCOUNTS {
        return Err("account_limit");
    }
    if !valid_username(&account.username)
        || !crate::crypto::valid_password_hash(&account.password_hash)
    {
        return Err("invalid_account");
    }
    if find_account(accounts, &account.username).is_some()
        || legacy.is_some_and(|c| c.username.eq_ignore_ascii_case(&account.username))
    {
        return Err("account_exists");
    }
    rotate_authorization(&mut account)?;
    accounts.push(account);
    if validate_accounts(accounts, legacy).is_err() {
        accounts.pop();
        return Err("last_administrator");
    }
    Ok(())
}

pub fn update_account(
    accounts: &mut [Account],
    legacy: Option<&Credential>,
    username: &str,
    role: Role,
    enabled: bool,
) -> Result<(), &'static str> {
    let index = accounts
        .iter()
        .position(|a| a.username.eq_ignore_ascii_case(username))
        .ok_or("account_not_found")?;
    if legacy.is_none()
        && !(enabled && role == Role::Administrator)
        && !accounts
            .iter()
            .enumerate()
            .any(|(i, a)| i != index && a.enabled && a.role == Role::Administrator)
    {
        return Err("last_administrator");
    }
    if accounts[index].role != role || accounts[index].enabled != enabled {
        rotate_authorization(&mut accounts[index])?;
    }
    accounts[index].role = role;
    accounts[index].enabled = enabled;
    Ok(())
}

pub fn delete_account(
    accounts: &mut Vec<Account>,
    legacy: Option<&Credential>,
    username: &str,
) -> Result<(), &'static str> {
    let index = accounts
        .iter()
        .position(|a| a.username.eq_ignore_ascii_case(username))
        .ok_or("account_not_found")?;
    if legacy.is_none()
        && accounts[index].enabled
        && accounts[index].role == Role::Administrator
        && !accounts
            .iter()
            .enumerate()
            .any(|(i, a)| i != index && a.enabled && a.role == Role::Administrator)
    {
        return Err("last_administrator");
    }
    accounts.remove(index);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(username: &str, role: Role, enabled: bool) -> Account {
        Account {
            username: username.into(),
            password_hash: crate::crypto::hash_password("test password").unwrap(),
            role,
            enabled,
            authorization_id: String::new(),
        }
    }

    #[test]
    fn rejects_ambiguous_or_invalid_accounts_and_requires_an_enabled_administrator() {
        let admin = account("Admin", Role::Administrator, true);
        let mut accounts = vec![admin.clone()];
        assert_eq!(
            add_account(&mut accounts, None, account("admin", Role::Viewer, true)),
            Err("account_exists")
        );
        assert_eq!(
            add_account(&mut accounts, None, account("bad name", Role::Viewer, true)),
            Err("invalid_account")
        );
        assert_eq!(validate_accounts(&accounts, None), Ok(()));
        accounts[0].enabled = false;
        assert_eq!(validate_accounts(&accounts, None), Err("invalid_state"));
        let legacy = Credential {
            username: "Legacy".into(),
            password_hash: admin.password_hash,
            legacy_salt: None,
            case_insensitive_username: false,
        };
        assert_eq!(validate_accounts(&accounts, Some(&legacy)), Ok(()));
        assert_eq!(
            add_account(
                &mut accounts,
                Some(&legacy),
                account("legacy", Role::Viewer, true)
            ),
            Err("account_exists")
        );
    }

    #[test]
    fn last_administrator_cannot_be_disabled_demoted_or_deleted() {
        let mut accounts = vec![
            account("one", Role::Administrator, true),
            account("two", Role::Viewer, true),
        ];
        assert_eq!(
            update_account(&mut accounts, None, "ONE", Role::Viewer, true),
            Err("last_administrator")
        );
        assert_eq!(
            update_account(&mut accounts, None, "one", Role::Administrator, false),
            Err("last_administrator")
        );
        assert_eq!(
            delete_account(&mut accounts, None, "one"),
            Err("last_administrator")
        );
        update_account(&mut accounts, None, "two", Role::Administrator, true).unwrap();
        delete_account(&mut accounts, None, "one").unwrap();
        assert_eq!(accounts[0].username, "two");
    }

    #[test]
    fn roles_are_fixed_and_scopes_intersect_host_policy() {
        let allowed = [
            "configuration.read",
            "application.launch",
            "configuration.write",
            "new.scope",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert_eq!(
            role_scopes(Role::Viewer, &allowed),
            ["configuration.read".to_owned()].into_iter().collect()
        );
        assert_eq!(
            role_scopes(Role::Operator, &allowed),
            [
                "configuration.read".to_owned(),
                "application.launch".to_owned()
            ]
            .into_iter()
            .collect()
        );
        assert_eq!(role_scopes(Role::Administrator, &allowed), allowed);
        assert_eq!(
            serde_json::to_string(&Role::Administrator).unwrap(),
            "\"administrator\""
        );
        assert!(serde_json::from_str::<Role>("\"superuser\"").is_err());
    }

    #[test]
    fn legacy_credential_counts_toward_the_limit_and_can_be_migrated() {
        let template = account("legacy", Role::Administrator, true);
        let legacy = Credential {
            username: template.username.clone(),
            password_hash: template.password_hash.clone(),
            legacy_salt: None,
            case_insensitive_username: false,
        };
        let mut accounts: Vec<Account> = (0..MAX_ACCOUNTS - 1)
            .map(|i| {
                let mut account = template.clone();
                account.username = format!("user{i}");
                account.role = Role::Viewer;
                account
            })
            .collect();
        assert_eq!(validate_accounts(&accounts, Some(&legacy)), Ok(()));
        let mut excess = template.clone();
        excess.username = "excess".into();
        assert_eq!(
            add_account(&mut accounts, Some(&legacy), excess.clone()),
            Err("account_limit")
        );
        accounts.push(excess);
        assert_eq!(
            validate_accounts(&accounts, Some(&legacy)),
            Err("invalid_state")
        );
        accounts.pop();
        // Atomically replace the legacy credential with its account counterpart.
        add_account(&mut accounts, None, template).unwrap();
        assert_eq!(accounts.len(), MAX_ACCOUNTS);
        assert_eq!(validate_accounts(&accounts, None), Ok(()));
    }

    #[test]
    fn reverted_changes_and_recreated_accounts_do_not_revive_sessions() {
        let mut accounts = Vec::new();
        add_account(
            &mut accounts,
            None,
            account("admin", Role::Administrator, true),
        )
        .unwrap();
        let viewer = account("viewer", Role::Viewer, true);
        add_account(&mut accounts, None, viewer.clone()).unwrap();
        let first = credential_digest(find_account(&accounts, "viewer").unwrap());
        update_account(&mut accounts, None, "viewer", Role::Operator, true).unwrap();
        update_account(&mut accounts, None, "viewer", Role::Viewer, true).unwrap();
        assert_ne!(
            first,
            credential_digest(find_account(&accounts, "viewer").unwrap())
        );
        let before_disable = credential_digest(find_account(&accounts, "viewer").unwrap());
        update_account(&mut accounts, None, "viewer", Role::Viewer, false).unwrap();
        update_account(&mut accounts, None, "viewer", Role::Viewer, true).unwrap();
        assert_ne!(
            before_disable,
            credential_digest(find_account(&accounts, "viewer").unwrap())
        );
        let before_delete = credential_digest(find_account(&accounts, "viewer").unwrap());
        delete_account(&mut accounts, None, "viewer").unwrap();
        add_account(&mut accounts, None, viewer).unwrap();
        assert_ne!(
            before_delete,
            credential_digest(find_account(&accounts, "viewer").unwrap())
        );
    }

    #[test]
    fn session_binding_changes_for_security_relevant_account_changes() {
        let original = account("admin", Role::Administrator, true);
        let digest = credential_digest(&original);
        let mut changed = original.clone();
        changed.role = Role::Viewer;
        assert_ne!(credential_digest(&changed), digest);
        changed = original.clone();
        changed.enabled = false;
        assert_ne!(credential_digest(&changed), digest);
        changed = original.clone();
        changed.password_hash = crate::crypto::hash_password("another password").unwrap();
        assert_ne!(credential_digest(&changed), digest);
        changed = original;
        changed.username.push('2');
        assert_ne!(credential_digest(&changed), digest);
    }
}
