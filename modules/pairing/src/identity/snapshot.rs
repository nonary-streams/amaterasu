//! Validated committed identity material. Never serialize this native cache.
use crate::{
    crypto,
    protocol::{Document, Identity, MAX_CLIENTS, MAX_DOCUMENT_BYTES},
};
use amaterasu_identity::*;
use amaterasu_module_sdk::abi::{STATUS_INTERNAL_ERROR, Status};
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
use rsa::{Pkcs1v15Sign, Pss, RsaPrivateKey, pkcs8::DecodePrivateKey};
use sha2::{Digest, Sha256, Sha384, Sha512};
use std::fs::{self, File};
use std::io::{Read, Take};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};
use x509_cert::{Certificate, der::Encode};

const MAX_IDENTITY_PEM_BYTES: usize = 4096;

/// File-backed TLS identity selected by host configuration.
///
/// The private key remains inside pairing and is used only by the native
/// identity snapshot. It is never included in a response or persisted state.
pub struct ConfiguredIdentity {
    pub(crate) certificate_pem: String,
    pub(crate) private_key_pem: String,
    pub(crate) provider_revision: u64,
    pub(crate) minimum_validation_time: u64,
}

impl ConfiguredIdentity {
    pub fn load(
        certificate_path: &str,
        private_key_path: &str,
        provider_revision: u64,
        now: u64,
    ) -> Result<Self, &'static str> {
        let validation_time = super::provision::create_missing_identity_pair(
            certificate_path,
            private_key_path,
            now,
        )?;
        Self::load_existing(
            certificate_path,
            private_key_path,
            provider_revision,
            validation_time,
        )
    }

    pub(crate) fn load_existing(
        certificate_path: &str,
        private_key_path: &str,
        provider_revision: u64,
        validation_time: u64,
    ) -> Result<Self, &'static str> {
        if same_path(Path::new(certificate_path), Path::new(private_key_path)) {
            return Err("unsafe_identity_path");
        }
        let certificate_pem = read_identity_file(certificate_path)?;
        let private_key_pem = read_identity_file(private_key_path)?;
        let validation_identity = Identity {
            host_id: "00000000-0000-4000-8000-000000000000".into(),
            certificate_pem: certificate_pem.clone(),
            private_key_pem: private_key_pem.clone(),
        };
        crypto::validate_identity(&validation_identity, validation_time)
            .map_err(|_| "invalid_identity")?;
        Ok(Self {
            certificate_pem,
            private_key_pem,
            provider_revision,
            minimum_validation_time: validation_time,
        })
    }
}

struct CertificateRecord {
    der: Vec<u8>,
    not_before: u64,
    not_after: u64,
    permissions: crate::protocol::ClientPermissions,
}
impl CertificateRecord {
    fn parse(certificate: &Certificate) -> Result<Self, &'static str> {
        let der = certificate.to_der().map_err(|_| "invalid_state")?;
        if der.len() > MAX_CERTIFICATE_BYTES {
            return Err("invalid_state");
        }
        Ok(Self {
            der,
            permissions: crate::protocol::ClientPermissions::default(),
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
    permission_mode_enabled: bool,
}
impl Snapshot {
    pub fn prepare(document: &Document, now: u64) -> Result<Option<Arc<Self>>, &'static str> {
        document.validate(now)?;
        let Some(identity) = &document.identity else {
            return Ok(None);
        };
        Self::prepare_material(
            document,
            &identity.certificate_pem,
            &identity.private_key_pem,
            now,
        )
        .map(Some)
    }

    pub fn prepare_with_configured_identity(
        document: &Document,
        now: u64,
        configured_identity: &ConfiguredIdentity,
    ) -> Result<Arc<Self>, &'static str> {
        // Keep the durable pairing document as the source of host ID and paired
        // clients. The configured pair replaces only the native TLS identity.
        if document.clients.len() > MAX_CLIENTS
            || serde_json::to_vec(document).map_or(true, |bytes| bytes.len() > MAX_DOCUMENT_BYTES)
            || (document.identity.is_none() && !document.clients.is_empty())
        {
            return Err("invalid_state");
        }
        let validation_document = Document {
            identity: Some(Identity {
                host_id: document
                    .identity
                    .as_ref()
                    .map(|identity| identity.host_id.clone())
                    .unwrap_or_else(|| "00000000-0000-4000-8000-000000000000".into()),
                certificate_pem: configured_identity.certificate_pem.clone(),
                private_key_pem: configured_identity.private_key_pem.clone(),
            }),
            clients: document.clients.clone(),
            permission_mode_enabled: document.permission_mode_enabled,
        };
        validation_document.validate(now)?;
        Self::prepare_material(
            document,
            &configured_identity.certificate_pem,
            &configured_identity.private_key_pem,
            now,
        )
    }

    fn prepare_material(
        document: &Document,
        certificate_pem: &str,
        private_key_pem: &str,
        now: u64,
    ) -> Result<Arc<Self>, &'static str> {
        let parsed_host = crypto::parse_certificate(certificate_pem, Some(now))?;
        let host = CertificateRecord::parse(&parsed_host.certificate)?;
        let key = RsaPrivateKey::from_pkcs8_pem(private_key_pem).map_err(|_| "invalid_state")?;
        if parsed_host.public_key != rsa::RsaPublicKey::from(&key) {
            return Err("invalid_state");
        }
        let clients = document
            .clients
            .iter()
            .map(|client| {
                let mut record = CertificateRecord::parse(
                    &crypto::parse_certificate(&client.certificate_pem, None)?.certificate,
                )?;
                record.permissions = client.permissions;
                Ok(record)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Arc::new(Self {
            host,
            key,
            clients,
            permission_mode_enabled: document.permission_mode_enabled,
        }))
    }

    pub fn certificate(&self) -> &[u8] {
        &self.host.der
    }

    pub fn check_permission(&self, der: &[u8], now: u64, permission: u32) -> Result<(), Status> {
        if !matches!(
            permission,
            PERMISSION_LIST | PERMISSION_LAUNCH | PERMISSION_VIEW | PERMISSION_TERMINATE
        ) {
            return Err(amaterasu_module_sdk::abi::STATUS_INVALID_ARGUMENT);
        }
        let client = self
            .clients
            .iter()
            .find(|client| client.der == der && client.valid(now))
            .ok_or(IDENTITY_UNTRUSTED)?;
        let p = client.permissions;
        let allowed = !self.permission_mode_enabled
            || match permission {
                PERMISSION_LIST => p.list_apps || p.view_streams || p.launch_apps,
                PERMISSION_VIEW => p.view_streams || p.launch_apps,
                PERMISSION_LAUNCH => p.launch_apps,
                PERMISSION_TERMINATE => p.terminate_streams,
                _ => unreachable!(),
            };
        if allowed {
            Ok(())
        } else {
            Err(IDENTITY_FORBIDDEN)
        }
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

struct SnapshotCacheEntry {
    pairing_revision: u64,
    configuration_revision: u64,
    snapshot: Option<Arc<Snapshot>>,
}

#[derive(Default)]
pub struct Shared {
    snapshot: RwLock<Option<SnapshotCacheEntry>>,
    latest_time: AtomicU64,
}
impl Shared {
    pub fn invalidate(&self) {
        if let Ok(mut snapshot) = self.snapshot.write() {
            *snapshot = None;
        }
    }
    #[cfg(test)]
    pub fn publish(
        &self,
        revision: u64,
        snapshot: Option<Arc<Snapshot>>,
    ) -> Result<(), &'static str> {
        self.publish_with_configuration_revision(revision, 0, snapshot)
    }
    pub fn publish_with_configuration_revision(
        &self,
        pairing_revision: u64,
        configuration_revision: u64,
        snapshot: Option<Arc<Snapshot>>,
    ) -> Result<(), &'static str> {
        let mut current = self
            .snapshot
            .write()
            .map_err(|_| "identity_cache_unavailable")?;
        if current.as_ref().is_some_and(|previous| {
            previous.pairing_revision == pairing_revision
                && previous.configuration_revision == configuration_revision
        }) {
            return Ok(());
        }
        *current = Some(SnapshotCacheEntry {
            pairing_revision,
            configuration_revision,
            snapshot,
        });
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
            .and_then(|entry| entry.snapshot.clone())
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
                    .and_then(|entry| entry.snapshot.as_ref())
                    .is_some_and(|current| Arc::ptr_eq(current, snapshot))
            })
    }
}

pub(super) fn read_identity_file(path_value: &str) -> Result<String, &'static str> {
    let path = Path::new(path_value);
    validate_identity_file_path(path)?;

    // Revalidate immediately before and after opening. Host configuration has
    // already checked effective paths, but those paths may change before use.
    validate_identity_file_path(path)?;
    let mut file = open_identity_file(path).map_err(|_| "identity_file_unavailable")?;
    validate_identity_file_path(path)?;
    let metadata = file.metadata().map_err(|_| "identity_file_unavailable")?;
    if is_reparse_or_symlink(&metadata)
        || !metadata.is_file()
        || metadata.len() > MAX_IDENTITY_PEM_BYTES as u64
    {
        return Err("invalid_identity_file");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    let mut bounded_file: Take<&mut File> = (&mut file).take((MAX_IDENTITY_PEM_BYTES + 1) as u64);
    bounded_file
        .read_to_end(&mut bytes)
        .map_err(|_| "identity_file_unavailable")?;
    if bytes.is_empty() || bytes.len() > MAX_IDENTITY_PEM_BYTES {
        return Err("invalid_identity_file");
    }
    String::from_utf8(bytes).map_err(|_| "invalid_identity_file")
}

fn open_identity_file(path: &Path) -> std::io::Result<File> {
    #[cfg(windows)]
    {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;

        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        options.open(path)
    }
    #[cfg(all(
        unix,
        any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        )
    ))]
    {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;

        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(any(target_os = "linux", target_os = "android"))]
        const O_NOFOLLOW: i32 = 0x0002_0000;
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))]
        const O_NOFOLLOW: i32 = 0x0000_0100;
        options.custom_flags(O_NOFOLLOW);
        options.open(path)
    }
    #[cfg(all(
        unix,
        not(any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))
    ))]
    {
        let _ = path;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "final-component no-follow is unsupported on this Unix target",
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no-follow identity opens are unsupported on this platform",
        ))
    }
}

fn validate_identity_file_path(path: &Path) -> Result<(), &'static str> {
    if !path.is_absolute()
        || path.as_os_str().to_string_lossy().len() > 4096
        || path.to_string_lossy().chars().any(char::is_control)
    {
        return Err("unsafe_identity_path");
    }
    let components: Vec<_> = path.components().collect();
    if components.is_empty() {
        return Err("unsafe_identity_path");
    }
    let mut current = PathBuf::new();
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::Normal(name) => current.push(name),
            Component::CurDir | Component::ParentDir => return Err("unsafe_identity_path"),
        }
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        let metadata = fs::symlink_metadata(&current).map_err(|_| "identity_file_unavailable")?;
        if is_reparse_or_symlink(&metadata) {
            return Err("unsafe_identity_path");
        }
        if index + 1 < components.len() && !metadata.is_dir() {
            return Err("unsafe_identity_path");
        }
        if index + 1 == components.len() && !metadata.is_file() {
            return Err("invalid_identity_file");
        }
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| "identity_file_unavailable")?;
    if !same_path(&canonical, path) {
        return Err("unsafe_identity_path");
    }
    Ok(())
}

fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        let normalize = |path: &Path| {
            let value = path.to_string_lossy();
            value
                .strip_prefix(r"\\?\")
                .unwrap_or(value.as_ref())
                .replace('/', "\\")
        };
        normalize(left).eq_ignore_ascii_case(&normalize(right))
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}
