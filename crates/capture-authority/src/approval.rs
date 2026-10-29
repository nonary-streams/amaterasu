//! Exact, machine-approved identity of one privileged package operation.
use amaterasu_package::{VerifiedPrivilegedRequest, sha256};
use serde::{Deserialize, Serialize};

pub const APPROVAL_SCHEMA: u32 = 1;

/// A record is only authoritative when read from the protected approval store.
/// Deserializing this structure from an ordinary user file grants nothing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRecord {
    pub schema: u32,
    pub request: VerifiedPrivilegedRequest,
    pub worker_sha256: String,
    pub uid: u32,
    pub gid: u32,
}

pub fn validate_approval(record: &ApprovalRecord) -> Result<(), String> {
    if record.schema != APPROVAL_SCHEMA || record.request.schema != 1 {
        return Err("unsupported privileged approval schema".into());
    }
    if [record.uid, record.gid]
        .iter()
        .any(|id| *id == 0 || *id == u32::MAX)
    {
        return Err("privileged capture workers require explicit non-root identities".into());
    }
    let request = &record.request;
    for digest in [
        &record.worker_sha256,
        &request.manifest_digest,
        &request.signer_fingerprint,
        &request.component_sha256,
        &request.helper.sha256,
    ] {
        validate_digest(digest)?;
    }
    if !amaterasu_contracts::valid_name(&request.component)
        || request.component == "amaterasu.console"
    {
        return Err("invalid privileged component identity".into());
    }
    if !request.target.starts_with("linux-")
        || request.target.len() > 128
        || !request
            .target
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("privileged capture requires an exact Linux target".into());
    }
    if request.resource.drm_card > 63 {
        return Err("privileged DRM card must be within 0..63".into());
    }
    let helper = &request.helper;
    if !amaterasu_contracts::valid_name(&helper.file)
        || helper.file.starts_with('.')
        || helper.file.contains('/')
        || helper.size == 0
        || helper.size > amaterasu_package::MAX_PRIVILEGED_HELPER_BYTES
    {
        return Err("invalid privileged helper artifact".into());
    }
    Ok(())
}

/// Hashes the canonical typed record, binding every approval field together.
/// Human confirmation and runtime lookup both use this identifier.
pub fn approval_id(record: &ApprovalRecord) -> Result<String, String> {
    validate_approval(record)?;
    let encoded = serde_json::to_vec(record).map_err(|error| error.to_string())?;
    Ok(sha256(&encoded))
}

pub(crate) fn validate_digest(digest: &str) -> Result<(), String> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("approval identifier must be canonical SHA-256".into());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use amaterasu_package::{
        PrivilegedHelperArtifact, PrivilegedOperation, PrivilegedResourceScope,
    };

    pub(crate) fn approval() -> ApprovalRecord {
        ApprovalRecord {
            schema: 1,
            request: VerifiedPrivilegedRequest {
                schema: 1,
                manifest_digest: "a".repeat(64),
                signer_fingerprint: "b".repeat(64),
                component: "example.capture".into(),
                component_sha256: "c".repeat(64),
                target: "linux-x86_64".into(),
                operation: PrivilegedOperation::VkmsFrameExportV1,
                resource: PrivilegedResourceScope { drm_card: 0 },
                helper: PrivilegedHelperArtifact {
                    file: "capture-helper".into(),
                    sha256: "d".repeat(64),
                    size: 1024,
                },
            },
            worker_sha256: "e".repeat(64),
            uid: 1000,
            gid: 1000,
        }
    }

    #[test]
    fn identity_binds_every_authority_field() {
        let initial = approval();
        let id = approval_id(&initial).unwrap();
        let mut changes = Vec::new();
        let mut changed = initial.clone();
        changed.request.manifest_digest = "f".repeat(64);
        changes.push(changed);
        let mut changed = initial.clone();
        changed.request.signer_fingerprint = "f".repeat(64);
        changes.push(changed);
        let mut changed = initial.clone();
        changed.request.component = "example.other".into();
        changes.push(changed);
        let mut changed = initial.clone();
        changed.request.component_sha256 = "f".repeat(64);
        changes.push(changed);
        let mut changed = initial.clone();
        changed.request.target = "linux-aarch64".into();
        changes.push(changed);
        let mut changed = initial.clone();
        changed.request.resource.drm_card = 1;
        changes.push(changed);
        let mut changed = initial.clone();
        changed.request.helper.sha256 = "f".repeat(64);
        changes.push(changed);
        let mut changed = initial.clone();
        changed.request.helper.size += 1;
        changes.push(changed);
        let mut changed = initial.clone();
        changed.request.helper.file = "other-helper".into();
        changes.push(changed);
        let mut changed = initial.clone();
        changed.worker_sha256 = "f".repeat(64);
        changes.push(changed);
        let mut changed = initial.clone();
        changed.uid += 1;
        changes.push(changed);
        let mut changed = initial.clone();
        changed.gid += 1;
        changes.push(changed);
        for changed in changes {
            assert_ne!(id, approval_id(&changed).unwrap());
        }
    }

    #[test]
    fn malformed_or_broad_approvals_are_refused() {
        let mut record = approval();
        record.uid = 0;
        assert!(approval_id(&record).is_err());
        record = approval();
        record.gid = 0;
        assert!(approval_id(&record).is_err());
        record = approval();
        record.uid = u32::MAX;
        assert!(approval_id(&record).is_err());
        record = approval();
        record.gid = u32::MAX;
        assert!(approval_id(&record).is_err());
        record = approval();
        record.worker_sha256 = "A".repeat(64);
        assert!(approval_id(&record).is_err());
        record = approval();
        record.request.helper.file = "../helper".into();
        assert!(approval_id(&record).is_err());
        record = approval();
        record.request.resource.drm_card = 64;
        assert!(approval_id(&record).is_err());
        record = approval();
        record.request.target = "linux-*".into();
        assert!(approval_id(&record).is_err());
    }
}
