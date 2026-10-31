//! Bounded, one-use coordinator permits. A permit is an opaque bearer secret,
//! but it is accepted only for its authenticated caller and exact binding.
//!
//! This type performs no I/O. Persist the updated book before returning a newly
//! issued token or executing a consumed permit; otherwise a crash could lose
//! revocation or permit the same effect twice. Protect persisted tokens as secrets.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const MAX_PERMITS: usize = 256;
const MAX_ID_BYTES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermitBinding {
    pub epoch: String,
    pub session_id: String,
    pub stream_id: String,
    pub generation: u64,
    pub caller: String,
    pub action: String,
    pub operation_id: String,
    /// Identifies the immutable, coordinator-approved plan, not caller authority.
    pub plan: String,
}

impl PermitBinding {
    fn valid(&self) -> bool {
        [
            &self.epoch,
            &self.session_id,
            &self.stream_id,
            &self.caller,
            &self.action,
            &self.operation_id,
            &self.plan,
        ]
        .into_iter()
        .all(|value| {
            !value.is_empty() && value.len() <= MAX_ID_BYTES && !value.chars().any(char::is_control)
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Permit {
    binding: PermitBinding,
    expires_at: u64,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermitBook {
    permits: BTreeMap<String, Permit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermitError {
    InvalidBinding,
    InvalidExpiry,
    Capacity,
    RandomUnavailable,
    NotAuthorized,
    InvalidBook,
}

impl PermitBook {
    /// Check untrusted persisted state before using it after restart.
    pub fn validate(&self) -> Result<(), PermitError> {
        if self.permits.len() > MAX_PERMITS
            || self.permits.iter().any(|(token, permit)| {
                token.len() != 32
                    || !token
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    || !permit.binding.valid()
                    || permit.expires_at == 0
            })
        {
            return Err(PermitError::InvalidBook);
        }
        Ok(())
    }

    /// Creates an unpredictable 128-bit token, bound to one admitted operation.
    pub fn issue(
        &mut self,
        binding: PermitBinding,
        expires_at: u64,
        now: u64,
    ) -> Result<String, PermitError> {
        self.validate()?;
        if !binding.valid() {
            return Err(PermitError::InvalidBinding);
        }
        if expires_at <= now {
            return Err(PermitError::InvalidExpiry);
        }
        self.permits.retain(|_, permit| permit.expires_at > now);
        if self.permits.len() >= MAX_PERMITS {
            return Err(PermitError::Capacity);
        }
        // Never overwrite an existing token, even in the event of a collision.
        for _ in 0..4 {
            let token = amaterasu_storage::random_identifier()
                .map_err(|_| PermitError::RandomUnavailable)?;
            if !self.permits.contains_key(&token) {
                self.permits.insert(
                    token.clone(),
                    Permit {
                        binding,
                        expires_at,
                    },
                );
                return Ok(token);
            }
        }
        Err(PermitError::RandomUnavailable)
    }

    /// Validate against broker-authenticated caller and a freshly resolved binding.
    /// Invalid, expired, or mismatched permits all return the same error.
    pub fn lookup(
        &self,
        token: &str,
        caller: &str,
        expected: &PermitBinding,
        now: u64,
    ) -> Result<(), PermitError> {
        self.validate()?;
        let permit = self.permits.get(token).ok_or(PermitError::NotAuthorized)?;
        if permit.expires_at <= now
            || caller != permit.binding.caller
            || expected != &permit.binding
        {
            return Err(PermitError::NotAuthorized);
        }
        Ok(())
    }

    /// Remove a permit after validating every binding. Persist before effects.
    pub fn consume(
        &mut self,
        token: &str,
        caller: &str,
        expected: &PermitBinding,
        now: u64,
    ) -> Result<(), PermitError> {
        self.lookup(token, caller, expected, now)?;
        self.permits.remove(token);
        Ok(())
    }

    /// Fence all permits for the exact stream generation, retaining other streams.
    pub fn revoke_generation(
        &mut self,
        epoch: &str,
        session_id: &str,
        stream_id: &str,
        generation: u64,
    ) -> usize {
        let before = self.permits.len();
        self.permits.retain(|_, permit| {
            let binding = &permit.binding;
            !(binding.epoch == epoch
                && binding.session_id == session_id
                && binding.stream_id == stream_id
                && binding.generation == generation)
        });
        before - self.permits.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> PermitBinding {
        PermitBinding {
            epoch: "epoch".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 7,
            caller: "caller".into(),
            action: "display/remove".into(),
            operation_id: "operation".into(),
            plan: "plan".into(),
        }
    }

    #[test]
    fn exact_authority_expiry_and_one_use_survive_roundtrip() {
        let mut book = PermitBook::default();
        let permit = book.issue(binding(), 20, 10).unwrap();
        let mut restored: PermitBook =
            serde_json::from_slice(&serde_json::to_vec(&book).unwrap()).unwrap();
        let mut other = binding();
        other.plan = "other".into();
        assert_eq!(
            restored.consume(&permit, "caller", &other, 11),
            Err(PermitError::NotAuthorized)
        );
        assert_eq!(
            restored.consume(&permit, "other", &binding(), 11),
            Err(PermitError::NotAuthorized)
        );
        assert_eq!(
            restored.lookup(&permit, "caller", &binding(), 20),
            Err(PermitError::NotAuthorized)
        );
        restored.consume(&permit, "caller", &binding(), 19).unwrap();
        assert_eq!(
            restored.consume(&permit, "caller", &binding(), 19),
            Err(PermitError::NotAuthorized)
        );
    }

    #[test]
    fn revoke_fences_only_exact_generation() {
        let mut book = PermitBook::default();
        let first = book.issue(binding(), 20, 10).unwrap();
        let mut next = binding();
        next.generation += 1;
        let second = book.issue(next.clone(), 20, 10).unwrap();
        assert_eq!(book.revoke_generation("epoch", "session", "stream", 7), 1);
        assert_eq!(
            book.lookup(&first, "caller", &binding(), 11),
            Err(PermitError::NotAuthorized)
        );
        assert_eq!(book.lookup(&second, "caller", &next, 11), Ok(()));
    }

    #[test]
    fn rejects_invalid_and_full_books_and_reclaims_expiry() {
        let mut book = PermitBook::default();
        let mut invalid = binding();
        invalid.action.clear();
        assert_eq!(
            book.issue(invalid, 20, 10),
            Err(PermitError::InvalidBinding)
        );
        assert_eq!(
            book.issue(binding(), 10, 10),
            Err(PermitError::InvalidExpiry)
        );
        for _ in 0..MAX_PERMITS {
            book.issue(binding(), 20, 10).unwrap();
        }
        assert_eq!(book.issue(binding(), 21, 10), Err(PermitError::Capacity));
        assert!(book.issue(binding(), 21, 20).is_ok());
        let corrupt: PermitBook = serde_json::from_str(r#"{"permits":{"bad":{"binding":{"epoch":"e","session_id":"s","stream_id":"t","generation":1,"caller":"c","action":"a","operation_id":"o","plan":"p"},"expires_at":10}}}"#).unwrap();
        assert_eq!(corrupt.validate(), Err(PermitError::InvalidBook));
    }
}
