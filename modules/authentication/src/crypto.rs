use argon2::password_hash::SaltString;
use argon2::{Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

fn password_hasher() -> Argon2<'static> {
    Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::default())
}

pub fn hash_password(password: &str) -> Result<String, &'static str> {
    let mut salt_bytes = [0u8; 16];
    getrandom::fill(&mut salt_bytes).map_err(|_| "entropy_unavailable")?;
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|_| "credential_failure")?;
    password_hasher()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| "credential_failure")
}

pub fn valid_password_hash(encoded: &str) -> bool {
    if encoded.len() > 256 {
        return false;
    }
    let Ok(hash) = PasswordHash::new(encoded) else {
        return false;
    };
    hash.algorithm.as_str() == "argon2id"
        && hash.version == Some(19)
        && hash.params.get_decimal("m") == Some(19_456)
        && hash.params.get_decimal("t") == Some(2)
        && hash.params.get_decimal("p") == Some(1)
        && hash.params.iter().count() == 3
        && hash.hash.is_some_and(|value| value.len() == 32)
        && hash.salt.is_some_and(|salt| {
            let mut bytes = [0; 64];
            salt.decode_b64(&mut bytes)
                .is_ok_and(|decoded| decoded.len() == 16)
        })
}

pub fn verify_password(password: &str, encoded: &str) -> bool {
    if !valid_password_hash(encoded) {
        return false;
    }
    PasswordHash::new(encoded).is_ok_and(|hash| {
        password_hasher()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

/// Verifies the sole predecessor format accepted by migration: lowercase
/// SHA-256 hex of the UTF-8 password followed by a 16-byte ASCII salt.
pub fn verify_legacy_password(password: &str, salt: &str, expected: &str) -> bool {
    if !valid_legacy_password_hash(expected) || !valid_legacy_salt(salt) {
        return false;
    }
    let mut digest = Sha256::new();
    digest.update(password.as_bytes());
    digest.update(salt.as_bytes());
    hexadecimal(&digest.finalize())
        .as_bytes()
        .ct_eq(expected.as_bytes())
        .into()
}

pub fn valid_legacy_password_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn valid_legacy_salt(value: &str) -> bool {
    value.len() == 16 && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

pub fn issue_bearer() -> Result<(String, String), &'static str> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "entropy_unavailable")?;
    let bearer = format!("ama_{}", hexadecimal(&bytes));
    let digest = token_digest(&bearer);
    Ok((bearer, digest))
}

pub fn token_digest(bearer: &str) -> String {
    hexadecimal(&Sha256::digest(bearer.as_bytes()))
}

fn hexadecimal(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char);
    }
    result
}
