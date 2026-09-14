//! Validated committed identity material. Never serialize this native cache.
use crate::{crypto, protocol::Document};
use amaterasu_identity::*;
use amaterasu_module_sdk::abi::{STATUS_INTERNAL_ERROR, Status};
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
use rsa::{Pkcs1v15Sign, Pss, RsaPrivateKey, pkcs8::DecodePrivateKey};
use sha2::{Digest, Sha256, Sha384, Sha512};
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};
use x509_cert::{Certificate, der::Encode};

struct CertificateRecord {
    der: Vec<u8>,
    not_before: u64,
    not_after: u64,
}
impl CertificateRecord {
    fn parse(certificate: &Certificate) -> Result<Self, &'static str> {
        let der = certificate.to_der().map_err(|_| "invalid_state")?;
        if der.len() > MAX_CERTIFICATE_BYTES {
            return Err("invalid_state");
        }
        Ok(Self {
            der,
            not_before: certificate
                .tbs_certificate
                .validity
                .not_before
                .to_unix_duration()
                .as_secs(),
            not_after: certificate
                .tbs_certificate
                .validity
                .not_after
                .to_unix_duration()
                .as_secs(),
        })
    }
    fn valid(&self, now: u64) -> bool {
        self.not_before <= now && now < self.not_after
    }
}

pub struct Snapshot {
    host: CertificateRecord,
    key: RsaPrivateKey,
    clients: Vec<CertificateRecord>,
}
impl Snapshot {
    pub fn prepare(document: &Document, now: u64) -> Result<Option<Arc<Self>>, &'static str> {
        document.validate(now)?;
        let Some(identity) = &document.identity else {
            return Ok(None);
        };
        let host = CertificateRecord::parse(
            &crypto::parse_certificate(&identity.certificate_pem, Some(now))?.certificate,
        )?;
        let key = RsaPrivateKey::from_pkcs8_pem(&identity.private_key_pem)
            .map_err(|_| "invalid_state")?;
        let clients = document
            .clients
            .iter()
            .map(|client| {
                CertificateRecord::parse(
                    &crypto::parse_certificate(&client.certificate_pem, None)?.certificate,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(Arc::new(Self { host, key, clients })))
    }

    pub fn certificate(&self) -> &[u8] {
        &self.host.der
    }

    pub fn verify_client(&self, der: &[u8], now: u64) -> bool {
        self.clients
            .iter()
            .any(|client| client.der == der && client.valid(now))
    }

    pub fn sign(&self, scheme: u32, message: &[u8]) -> Result<Vec<u8>, Status> {
        if message.len() > MAX_MESSAGE_BYTES {
            return Err(amaterasu_module_sdk::abi::STATUS_INVALID_ARGUMENT);
        }
        if !matches!(
            scheme,
            TLS_RSA_PKCS1_SHA256
                | TLS_RSA_PKCS1_SHA384
                | TLS_RSA_PKCS1_SHA512
                | TLS_RSA_PSS_RSAE_SHA256
                | TLS_RSA_PSS_RSAE_SHA384
                | TLS_RSA_PSS_RSAE_SHA512
        ) {
            return Err(IDENTITY_UNSUPPORTED);
        }
        let mut random = ChaCha20Rng::from_seed(
            crypto::random_bytes::<32>().map_err(|_| STATUS_INTERNAL_ERROR)?,
        );
        // RustCrypto performs private RSA blinding in every variant. PSS uses a
        // fresh digest-sized salt and the blinded private operation explicitly.
        let signature = match scheme {
            TLS_RSA_PKCS1_SHA256 => self.key.sign_with_rng(
                &mut random,
                Pkcs1v15Sign::new::<Sha256>(),
                &Sha256::digest(message),
            ),
            TLS_RSA_PKCS1_SHA384 => self.key.sign_with_rng(
                &mut random,
                Pkcs1v15Sign::new::<Sha384>(),
                &Sha384::digest(message),
            ),
            TLS_RSA_PKCS1_SHA512 => self.key.sign_with_rng(
                &mut random,
                Pkcs1v15Sign::new::<Sha512>(),
                &Sha512::digest(message),
            ),
            TLS_RSA_PSS_RSAE_SHA256 => self.key.sign_with_rng(
                &mut random,
                Pss::new_blinded::<Sha256>(),
                &Sha256::digest(message),
            ),
            TLS_RSA_PSS_RSAE_SHA384 => self.key.sign_with_rng(
                &mut random,
                Pss::new_blinded::<Sha384>(),
                &Sha384::digest(message),
            ),
            TLS_RSA_PSS_RSAE_SHA512 => self.key.sign_with_rng(
                &mut random,
                Pss::new_blinded::<Sha512>(),
                &Sha512::digest(message),
            ),
            _ => unreachable!("scheme checked"),
        }
        .map_err(|_| STATUS_INTERNAL_ERROR)?;
        if signature.len() > MAX_SIGNATURE_BYTES {
            return Err(STATUS_INTERNAL_ERROR);
        }
        Ok(signature)
    }
}

#[derive(Default)]
pub struct Shared {
    snapshot: RwLock<Option<(u64, Option<Arc<Snapshot>>)>>,
    latest_time: AtomicU64,
}
impl Shared {
    pub fn invalidate(&self) {
        if let Ok(mut snapshot) = self.snapshot.write() {
            *snapshot = None;
        }
    }
    pub fn publish(
        &self,
        revision: u64,
        snapshot: Option<Arc<Snapshot>>,
    ) -> Result<(), &'static str> {
        let mut current = self
            .snapshot
            .write()
            .map_err(|_| "identity_cache_unavailable")?;
        if current
            .as_ref()
            .is_some_and(|(previous, _)| *previous == revision)
        {
            return Ok(());
        }
        *current = Some((revision, snapshot));
        Ok(())
    }
    pub fn now(&self) -> Result<u64, Status> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| IDENTITY_UNAVAILABLE)?
            .as_secs();
        Ok(self.latest_time.fetch_max(now, Ordering::SeqCst).max(now))
    }
    pub fn get(&self, now: u64) -> Result<Arc<Snapshot>, Status> {
        let snapshot = self
            .snapshot
            .read()
            .map_err(|_| IDENTITY_UNAVAILABLE)?
            .as_ref()
            .and_then(|(_, snapshot)| snapshot.clone())
            .ok_or(IDENTITY_UNAVAILABLE)?;
        if !snapshot.host.valid(now) {
            return Err(IDENTITY_UNAVAILABLE);
        }
        Ok(snapshot)
    }
    /// Linearization check after cryptographic work. A load/write invalidation or
    /// snapshot replacement prevents the previously borrowed identity being used.
    pub fn current(&self, snapshot: &Arc<Snapshot>, now: u64) -> bool {
        snapshot.host.valid(now)
            && self.snapshot.read().is_ok_and(|current| {
                current
                    .as_ref()
                    .and_then(|(_, current)| current.as_ref())
                    .is_some_and(|current| Arc::ptr_eq(current, snapshot))
            })
    }
}
