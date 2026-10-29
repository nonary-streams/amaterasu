//! Signed, finite requests for operations performed by a privileged broker.
//!
//! These records describe requested authority; verifying one never grants it.
//! Native modules remain trusted code. Only the separate privileged service can
//! compare these identities with machine-protected approval and execute work.
use crate::{PackageResult, ReleaseManifest, read_bounded, sha256};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};

pub const PRIVILEGED_REQUEST_SCHEMA: u32 = 1;
pub const MAX_PRIVILEGED_REQUESTS: usize = 64;
pub const MAX_PRIVILEGED_HELPER_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivilegedOperation {
    VkmsFrameExportV1,
}

/// Exact Linux DRM card scope; there is deliberately no wildcard variant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivilegedResourceScope {
    pub drm_card: u32,
}

/// Helper bytes are part of the signed package, separately from loaded modules.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivilegedHelperArtifact {
    pub file: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivilegedRequest {
    pub schema: u32,
    pub component: String,
    pub operation: PrivilegedOperation,
    pub target: String,
    pub resource: PrivilegedResourceScope,
    pub helper: PrivilegedHelperArtifact,
}

/// Exact identity produced from a verified manifest and verified signing key.
///
/// Deserializing this type is useful for protected approval records; it does not
/// establish verification. A broker must compare every field against a fresh
/// result of package verification before treating an approval as applicable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedPrivilegedRequest {
    pub schema: u32,
    pub manifest_digest: String,
    pub signer_fingerprint: String,
    pub component: String,
    pub component_sha256: String,
    pub target: String,
    pub operation: PrivilegedOperation,
    pub resource: PrivilegedResourceScope,
    pub helper: PrivilegedHelperArtifact,
}

pub(crate) fn verify_privileged_requests(
    directory: &Path,
    manifest: &ReleaseManifest,
    manifest_digest: &str,
    signer_fingerprint: Option<&str>,
) -> PackageResult<Vec<VerifiedPrivilegedRequest>> {
    if manifest.privileged_requests.is_empty() {
        return Ok(Vec::new());
    }
    let signer = signer_fingerprint.ok_or("unsigned privileged requests rejected")?;
    if manifest.privileged_requests.len() > MAX_PRIVILEGED_REQUESTS {
        return Err("too many privileged requests".into());
    }
    let mut requests = BTreeSet::new();
    let mut artifacts = BTreeSet::new();
    let component_files: BTreeSet<_> = manifest
        .components
        .iter()
        .map(|component| component.file.as_str())
        .collect();
    let mut verified = Vec::new();
    for request in &manifest.privileged_requests {
        let component = validate_request(manifest, request)?;
        if !requests.insert((&request.component, request.operation)) {
            return Err("duplicate privileged component operation".into());
        }
        if component_files.contains(request.helper.file.as_str())
            || !artifacts.insert(&request.helper.file)
        {
            return Err("duplicate privileged helper artifact path".into());
        }
        verify_privileged_helper(directory, &request.helper)?;
        verified.push(VerifiedPrivilegedRequest {
            schema: PRIVILEGED_REQUEST_SCHEMA,
            manifest_digest: manifest_digest.to_owned(),
            signer_fingerprint: signer.to_owned(),
            component: request.component.clone(),
            component_sha256: component.sha256.clone(),
            target: request.target.clone(),
            operation: request.operation,
            resource: request.resource.clone(),
            helper: request.helper.clone(),
        });
    }
    Ok(verified)
}

fn validate_request<'a>(
    manifest: &'a ReleaseManifest,
    request: &PrivilegedRequest,
) -> PackageResult<&'a crate::ComponentManifest> {
    if request.schema != PRIVILEGED_REQUEST_SCHEMA {
        return Err("unsupported privileged request schema".into());
    }
    if request.target != manifest.target || !request.target.starts_with("linux-") {
        return Err("privileged operation requires the exact Linux package target".into());
    }
    if request.resource.drm_card > 63 {
        return Err("privileged DRM card must be within 0..63".into());
    }
    let component = manifest
        .components
        .iter()
        .find(|component| component.id == request.component && !component.runtime)
        .ok_or("privileged request requires a packaged feature component")?;
    validate_helper(&request.helper)?;
    Ok(component)
}

pub(crate) fn validate_helper(helper: &PrivilegedHelperArtifact) -> PackageResult<()> {
    if !(amaterasu_contracts::valid_name(&helper.file)
        && !helper.file.starts_with('.')
        && !helper.file.contains('/')
        && !matches!(helper.file.as_str(), "manifest.json" | "manifest.sig"))
    {
        return Err("privileged helper requires a simple artifact filename".into());
    }
    if helper.size == 0 || helper.size > MAX_PRIVILEGED_HELPER_BYTES {
        return Err("privileged helper exceeds artifact size bounds".into());
    }
    if helper.sha256.len() != 64
        || !helper
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("privileged helper digest must be canonical SHA-256".into());
    }
    Ok(())
}

/// Rechecks helper bytes against their signed identity at a deployment boundary.
/// This check grants no OS authority and does not install or execute the helper.
pub fn verify_privileged_helper(
    directory: &Path,
    helper: &PrivilegedHelperArtifact,
) -> PackageResult<()> {
    validate_helper(helper)?;
    let bytes = read_bounded(&directory.join(&helper.file), MAX_PRIVILEGED_HELPER_BYTES)?;
    if bytes.len() as u64 != helper.size || sha256(&bytes) != helper.sha256 {
        return Err("privileged helper artifact integrity failed".into());
    }
    Ok(())
}
