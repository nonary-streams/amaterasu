use crate::protocol::Identity;
use aes::{
    Aes128,
    cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray},
};
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
use rsa::{
    RsaPrivateKey, RsaPublicKey,
    pkcs1v15::{Signature, SigningKey, VerifyingKey},
    pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey, LineEnding},
    signature::{RandomizedSigner, SignatureEncoding, Verifier},
    traits::PublicKeyParts,
};
use sha2::{Digest, Sha256};
use std::{str::FromStr, time::Duration};
use x509_cert::{
    Certificate,
    builder::{Builder, CertificateBuilder, Profile},
    der::{DecodePem, Encode, EncodePem},
    name::Name,
    serial_number::SerialNumber,
    spki::SubjectPublicKeyInfoOwned,
    time::Validity,
};

pub struct PeerCertificate {
    pub certificate: Certificate,
    pub public_key: RsaPublicKey,
    pub fingerprint: String,
}

pub fn generate_identity() -> Result<Identity, &'static str> {
    let mut random = ChaCha20Rng::from_seed(random_bytes::<32>()?);
    let key = RsaPrivateKey::new(&mut random, 2048).map_err(|_| "identity_failure")?;
    let signer = SigningKey::<Sha256>::new(key.clone());
    let public_der = RsaPublicKey::from(&key)
        .to_public_key_der()
        .map_err(|_| "identity_failure")?;
    let public = SubjectPublicKeyInfoOwned::try_from(public_der.as_bytes())
        .map_err(|_| "identity_failure")?;
    let validity = Validity::from_now(Duration::from_secs(20 * 365 * 24 * 60 * 60))
        .map_err(|_| "clock_failure")?;
    let subject = Name::from_str("CN=Amaterasu GameStream Host").map_err(|_| "identity_failure")?;
    let serial = SerialNumber::new(&random_bytes::<16>()?).map_err(|_| "identity_failure")?;
    let profile = Profile::Leaf {
        issuer: subject.clone(),
        enable_key_agreement: false,
        enable_key_encipherment: true,
    };
    let builder = CertificateBuilder::new(profile, serial, validity, subject, public, &signer)
        .map_err(|_| "identity_failure")?;
    let certificate = builder
        .build_with_rng::<Signature>(&mut random)
        .map_err(|_| "identity_failure")?;
    Ok(Identity {
        host_id: host_id()?,
        certificate_pem: certificate
            .to_pem(LineEnding::LF)
            .map_err(|_| "identity_failure")?,
        private_key_pem: key
            .to_pkcs8_pem(LineEnding::LF)
            .map_err(|_| "identity_failure")?
            .to_string(),
    })
}

pub fn import_identity(
    certificate_pem: String,
    private_key_pem: String,
    existing_host_id: Option<String>,
    now: u64,
) -> Result<Identity, &'static str> {
    let identity = Identity {
        host_id: existing_host_id.map_or_else(host_id, Ok)?,
        certificate_pem,
        private_key_pem,
    };
    validate_identity(&identity, now)?;
    Ok(identity)
}

pub fn validate_identity(identity: &Identity, now: u64) -> Result<(), &'static str> {
    if identity.private_key_pem.len() > 4096 {
        return Err("invalid_identity");
    }
    let cert = parse_certificate(&identity.certificate_pem, Some(now))?;
    let key =
        RsaPrivateKey::from_pkcs8_pem(&identity.private_key_pem).map_err(|_| "invalid_identity")?;
    if RsaPublicKey::from(&key) != cert.public_key {
        return Err("invalid_identity");
    }
    key.validate().map_err(|_| "invalid_identity")
}

pub fn parse_certificate(pem: &str, now: Option<u64>) -> Result<PeerCertificate, &'static str> {
    if pem.is_empty() || pem.len() > 4096 {
        return Err("invalid_certificate");
    }
    let certificate = Certificate::from_pem(pem).map_err(|_| "invalid_certificate")?;
    let tbs = &certificate.tbs_certificate;
    if tbs.subject != tbs.issuer
        || certificate.signature_algorithm != tbs.signature
        || certificate.signature_algorithm.oid.to_string() != "1.2.840.113549.1.1.11"
    {
        return Err("invalid_certificate");
    }
    if let Some(now) = now
        && (tbs.validity.not_before.to_unix_duration().as_secs() > now
            || tbs.validity.not_after.to_unix_duration().as_secs() <= now)
    {
        return Err("invalid_certificate");
    }
    let public_key = RsaPublicKey::from_public_key_der(
        &tbs.subject_public_key_info
            .to_der()
            .map_err(|_| "invalid_certificate")?,
    )
    .map_err(|_| "invalid_certificate")?;
    if !(2048..=4096).contains(&public_key.n().bits()) {
        return Err("invalid_certificate");
    }
    verify(
        &public_key,
        &tbs.to_der().map_err(|_| "invalid_certificate")?,
        certificate
            .signature
            .as_bytes()
            .ok_or("invalid_certificate")?,
    )?;
    let fingerprint = hex(&Sha256::digest(
        certificate.to_der().map_err(|_| "invalid_certificate")?,
    ));
    Ok(PeerCertificate {
        certificate,
        public_key,
        fingerprint,
    })
}

pub fn signature_bytes(certificate: &Certificate) -> Result<&[u8], &'static str> {
    certificate
        .signature
        .as_bytes()
        .ok_or("invalid_certificate")
}
pub fn sign(identity: &Identity, message: &[u8]) -> Result<Vec<u8>, &'static str> {
    let key =
        RsaPrivateKey::from_pkcs8_pem(&identity.private_key_pem).map_err(|_| "invalid_identity")?;
    let mut random = ChaCha20Rng::from_seed(random_bytes::<32>()?);
    Ok(SigningKey::<Sha256>::new(key)
        .try_sign_with_rng(&mut random, message)
        .map_err(|_| "signature_failure")?
        .to_vec())
}
pub fn verify(key: &RsaPublicKey, message: &[u8], signature: &[u8]) -> Result<(), &'static str> {
    let signature = Signature::try_from(signature).map_err(|_| "pairing_failed")?;
    VerifyingKey::<Sha256>::new(key.clone())
        .verify(message, &signature)
        .map_err(|_| "pairing_failed")
}
pub fn digest(parts: &[&[u8]]) -> [u8; 32] {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(part);
    }
    hash.finalize().into()
}
pub fn random_bytes<const N: usize>() -> Result<[u8; N], &'static str> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|_| "entropy_unavailable")?;
    Ok(bytes)
}
pub fn crypt(key: &[u8; 16], input: &[u8], encrypt: bool) -> Result<Vec<u8>, &'static str> {
    if input.is_empty() || !input.len().is_multiple_of(16) {
        return Err("invalid_request");
    }
    // ECB without padding is required by Moonlight's Gen7 pairing transcript.
    // It is confined to these fixed-size protocol blocks, not general encryption.
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut output = input.to_vec();
    for block in output.as_chunks_mut::<16>().0 {
        if encrypt {
            cipher.encrypt_block(GenericArray::from_mut_slice(block));
        } else {
            cipher.decrypt_block(GenericArray::from_mut_slice(block));
        }
    }
    Ok(output)
}
pub fn hex(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(ALPHABET[(b >> 4) as usize] as char);
        out.push(ALPHABET[(b & 15) as usize] as char);
    }
    out
}
pub fn unhex(value: &str) -> Result<Vec<u8>, &'static str> {
    if !value.len().is_multiple_of(2) {
        return Err("invalid_request");
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).ok_or("invalid_request")?;
            let low = (pair[1] as char).to_digit(16).ok_or("invalid_request")?;
            Ok((high * 16 + low) as u8)
        })
        .collect()
}
fn host_id() -> Result<String, &'static str> {
    let mut bytes = random_bytes::<16>()?;
    bytes[6] = (bytes[6] & 15) | 0x40;
    bytes[8] = (bytes[8] & 63) | 0x80;
    let hex = hex(&bytes);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}
