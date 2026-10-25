//! Bounded retrieval with a deterministic layout and no redirect following.
use super::{
    ApprovedRepository, Candidate, CandidateKind, RepositoryStore, VerifiedCatalog, valid_segment,
    verify_candidate,
};
use crate::{
    BootstrapConfig, PackageResult, ReleaseManifest, create_private_directory, sha256, web,
};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Duration;
use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig};

const MAX_NATIVE_SET_BYTES: u64 = 512 * 1024 * 1024;
const MAX_NATIVE_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;

pub trait RepositoryTransport {
    fn get(&self, relative: &str, maximum: u64) -> PackageResult<Vec<u8>>;
}

/// HTTPS transport for one administrator-approved location. Redirects are
/// refused, so a response cannot silently switch the download origin.
pub struct HttpsRepository {
    base: String,
    agent: Agent,
}

impl HttpsRepository {
    pub fn new(approved: &ApprovedRepository) -> PackageResult<Self> {
        approved.validate()?;
        let config = Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(10)))
            .max_response_header_size(8 * 1024)
            .input_buffer_size(8 * 1024)
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .build();
        Ok(Self {
            base: approved.location.trim_end_matches('/').into(),
            agent: Agent::new_with_config(config),
        })
    }
}

impl RepositoryTransport for HttpsRepository {
    fn get(&self, relative: &str, maximum: u64) -> PackageResult<Vec<u8>> {
        if !relative.split('/').all(valid_segment) {
            return Err("invalid repository path or byte limit".into());
        }
        let url = format!("{}/{relative}", self.base);
        let response = self
            .agent
            .get(&url)
            .call()
            .map_err(|error| error.to_string())?;
        if response.status().as_u16() != 200 {
            return Err("repository response was not successful".into());
        }
        let mut bytes = Vec::new();
        response
            .into_body()
            .as_reader()
            .take(maximum + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > maximum {
            return Err("repository response exceeds byte limit".into());
        }
        Ok(bytes)
    }
}

pub fn refresh_catalog(
    store: &RepositoryStore,
    repository_id: &str,
    transport: &impl RepositoryTransport,
    target: &str,
    now: u64,
) -> PackageResult<()> {
    store.approved(repository_id)?;
    let raw = transport.get("catalog.json", 256 * 1024)?;
    let signature = transport.get("catalog.sig", 128)?;
    let signature = std::str::from_utf8(&signature).map_err(|error| error.to_string())?;
    store.accept_catalog(repository_id, &raw, signature, target, now)
}

/// Downloads into a new local directory and verifies the complete payload.
/// This is preparation only: it never loads a library or changes selection.
pub fn download_candidate(
    transport: &impl RepositoryTransport,
    approved: &ApprovedRepository,
    catalog: &VerifiedCatalog,
    candidate: &Candidate,
    output: &Path,
    bootstrap: &BootstrapConfig,
    now: u64,
) -> PackageResult<()> {
    approved.validate()?;
    if output.try_exists().map_err(|error| error.to_string())? {
        return Err("candidate output already exists".into());
    }
    let kind = match candidate.kind {
        CandidateKind::NativeSet => "native",
        CandidateKind::WebBundle => "web",
    };
    if !valid_segment(&candidate.implementation) || !valid_segment(&candidate.version) {
        return Err("invalid candidate path segment".into());
    }
    let prefix = format!("{kind}/{}/{}", candidate.implementation, candidate.version);
    let manifest = transport.get(&format!("{prefix}/manifest.json"), 1_048_576)?;
    if sha256(&manifest) != candidate.manifest_sha256 {
        return Err("downloaded manifest is not listed in the signed catalog".into());
    }
    let signature = transport.get(&format!("{prefix}/manifest.sig"), 4_096)?;
    let files = payload_files(candidate.kind, &manifest)?;
    fs::create_dir(output).map_err(|error| error.to_string())?;
    amaterasu_storage::write_atomically(&output.join("manifest.json"), &manifest)?;
    amaterasu_storage::write_atomically(&output.join("manifest.sig"), &signature)?;
    for (name, maximum) in files {
        let bytes = transport.get(&format!("{prefix}/{name}"), maximum.max(1))?;
        let path = output.join(&name);
        create_private_directory(path.parent().ok_or("candidate path has no parent")?)?;
        amaterasu_storage::write_atomically(&path, &bytes)?;
    }
    verify_candidate(approved, catalog, candidate, output, bootstrap, now)
}

fn payload_files(kind: CandidateKind, raw: &[u8]) -> PackageResult<Vec<(String, u64)>> {
    match kind {
        CandidateKind::WebBundle => {
            let manifest: web::Manifest =
                serde_json::from_slice(raw).map_err(|error| error.to_string())?;
            if manifest.files.len() > web::MAX_FILES {
                return Err("too many web assets".into());
            }
            let mut total = 0u64;
            manifest
                .files
                .into_iter()
                .map(|(name, asset)| {
                    web::validate_asset_name(&name)?;
                    total = total.saturating_add(asset.size);
                    if asset.size > web::MAX_ASSET_BYTES || total > web::MAX_BUNDLE_BYTES {
                        return Err("web download exceeds byte limit".into());
                    }
                    Ok((name, asset.size))
                })
                .collect()
        }
        CandidateKind::NativeSet => {
            let manifest: ReleaseManifest =
                serde_json::from_slice(raw).map_err(|error| error.to_string())?;
            if manifest.components.len() > 64 {
                return Err("too many native artifacts".into());
            }
            let mut total = 0u64;
            manifest
                .components
                .into_iter()
                .map(|component| {
                    if !valid_segment(&component.file)
                        || matches!(component.file.as_str(), "manifest.json" | "manifest.sig")
                    {
                        return Err("invalid native artifact filename".into());
                    }
                    total = total.saturating_add(component.size);
                    if component.size > MAX_NATIVE_ARTIFACT_BYTES || total > MAX_NATIVE_SET_BYTES {
                        return Err("native download exceeds byte limit".into());
                    }
                    Ok((component.file, component.size))
                })
                .collect()
        }
    }
}
