//! Provider-neutral, exact capture target selection.
//!
//! Target identities remain private to providers. The public selector is a
//! domain-separated digest of a provider's canonical identity. Compatible
//! providers agree on that canonical identity and must resolve the selector to
//! exactly one currently observed target. An unavailable selector never permits
//! falling back to another output.
use sha2::{Digest, Sha256};

pub const EXACT_TARGET_PREFIX: &str = "exact-target:";

/// Derive an opaque selector without publishing a native target name or handle.
/// Providers own identity canonicalization (for example case folding).
pub fn exact_target_selector(canonical_identity: &str) -> Result<String, &'static str> {
    if canonical_identity.is_empty()
        || canonical_identity.len() > 4096
        || canonical_identity.chars().any(char::is_control)
    {
        return Err("invalid_capture_target_identity");
    }
    let mut digest = Sha256::new();
    digest.update(b"amaterasu.capture.exact-target/1\0");
    digest.update((canonical_identity.len() as u64).to_be_bytes());
    digest.update(canonical_identity.as_bytes());
    Ok(format!("{EXACT_TARGET_PREFIX}{:x}", digest.finalize()))
}

/// Validate an exact selector. This validates syntax, not target availability.
pub fn exact_target_key(selector: &str) -> Option<&str> {
    let key = selector.strip_prefix(EXACT_TARGET_PREFIX)?;
    (key.len() == 64
        && key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatible_providers_derive_same_opaque_target() {
        let selector = exact_target_selector("canonical-owned-target").unwrap();
        assert_eq!(exact_target_key(&selector).unwrap().len(), 64);
        assert_eq!(
            selector,
            exact_target_selector("canonical-owned-target").unwrap()
        );
        assert_ne!(selector, exact_target_selector("different-target").unwrap());
        assert!(!selector.contains("canonical-owned-target"));
    }

    #[test]
    fn malformed_selectors_and_invalid_private_identities_are_rejected() {
        for selector in ["", "exact-target:", "exact-target:abc", "output-1"] {
            assert_eq!(exact_target_key(selector), None);
        }
        assert_eq!(
            exact_target_key(&format!("exact-target:{}", "A".repeat(64))),
            None
        );
        for identity in ["", "target\n"] {
            assert!(exact_target_selector(identity).is_err());
        }
        assert!(exact_target_selector(&"x".repeat(4097)).is_err());
    }
}
