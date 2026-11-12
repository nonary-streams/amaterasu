//! What management reads from host-owned state to ask for a package activation.
//!
//! The host shell performs the activation and decides whether it is allowed.
//! Nothing here authorizes native code: the request names a cached candidate
//! the host verifies again, and the receipt only reports what the host did.
use crate::candidate_cache::PreparedCandidate;
use amaterasu_contracts::HostPackageActivation;
use serde_json::Value;
use std::path::Path;

const SELECTION_BYTES: u64 = 1024 * 1024;
const RECEIPT_BYTES: u64 = 4096;

/// Why an activation request could not be built from the current selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RequestRefusal {
    /// The selection is unreadable or names no active set.
    SelectionUnavailable,
    /// The selection already records a pending or trial set.
    ActivationInProgress,
}

/// Builds the exact request for a prepared candidate from the selection the
/// host wrote. The revision makes the host refuse it if the selection changes.
pub(crate) fn request(
    package_root: &Path,
    candidate: &PreparedCandidate,
) -> Result<HostPackageActivation, RequestRefusal> {
    let bytes =
        amaterasu_package::read_bounded(&package_root.join("selection.json"), SELECTION_BYTES)
            .map_err(|_| RequestRefusal::SelectionUnavailable)?;
    let selection: amaterasu_package::ActivationState =
        serde_json::from_slice(&bytes).map_err(|_| RequestRefusal::SelectionUnavailable)?;
    let active = selection
        .active
        .ok_or(RequestRefusal::SelectionUnavailable)?;
    if selection.pending.is_some() || selection.trial.is_some() {
        return Err(RequestRefusal::ActivationInProgress);
    }
    Ok(HostPackageActivation {
        repository_id: candidate.repository_id.clone(),
        catalog_digest: candidate.catalog_digest.clone(),
        manifest_digest: candidate.digest.clone(),
        expected_active: active,
        expected_revision: amaterasu_package::sha256(&bytes),
    })
}

/// How the host says its most recent activation attempt ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReceiptStatus {
    Activating,
    Committed,
    Failed,
}

/// The host's record of its most recent activation attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Receipt {
    /// The set the attempt was for.
    pub digest: String,
    pub status: ReceiptStatus,
}

/// Reads the receipt only when it is the private file the host writes.
pub(crate) fn receipt(package_root: &Path) -> Option<Receipt> {
    use std::os::unix::fs::MetadataExt;
    let path = package_root.join("data/state/package-activation-status.json");
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    // SAFETY: geteuid has no pointer or lifetime requirements.
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
    {
        return None;
    }
    let bytes = amaterasu_package::read_bounded(&path, RECEIPT_BYTES).ok()?;
    let receipt: Value = serde_json::from_slice(&bytes).ok()?;
    if receipt.get("schema").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    let digest = receipt.get("digest").and_then(Value::as_str)?;
    if !crate::package_status::is_sha256(digest) {
        return None;
    }
    let status = match receipt.get("status").and_then(Value::as_str)? {
        "activating" => ReceiptStatus::Activating,
        "committed" => ReceiptStatus::Committed,
        "failed" => ReceiptStatus::Failed,
        _ => return None,
    };
    Some(Receipt {
        digest: digest.into(),
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn temporary_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-host-activation-{}",
            amaterasu_package::random_identifier().unwrap()
        ));
        std::fs::create_dir_all(root.join("data/state")).unwrap();
        root
    }

    fn candidate() -> PreparedCandidate {
        PreparedCandidate {
            repository_id: "approved".into(),
            catalog_digest: "b".repeat(64),
            digest: "c".repeat(64),
        }
    }

    #[test]
    fn request_binds_the_candidate_to_the_exact_selection_bytes() {
        let root = temporary_root();
        assert_eq!(
            request(&root, &candidate()).unwrap_err(),
            RequestRefusal::SelectionUnavailable
        );
        let mut selection = amaterasu_package::ActivationState {
            active: Some("a".repeat(64)),
            ..Default::default()
        };
        let path = root.join("selection.json");
        amaterasu_package::write_json_atomically(&path, &selection).unwrap();
        let built = request(&root, &candidate()).unwrap();
        assert_eq!(built.repository_id, "approved");
        assert_eq!(built.catalog_digest, "b".repeat(64));
        assert_eq!(built.manifest_digest, "c".repeat(64));
        assert_eq!(built.expected_active, "a".repeat(64));
        assert_eq!(
            built.expected_revision,
            amaterasu_package::sha256(&std::fs::read(&path).unwrap())
        );

        selection.pending = Some("d".repeat(64));
        amaterasu_package::write_json_atomically(&path, &selection).unwrap();
        assert_eq!(
            request(&root, &candidate()).unwrap_err(),
            RequestRefusal::ActivationInProgress
        );
        selection.pending = None;
        selection.active = None;
        amaterasu_package::write_json_atomically(&path, &selection).unwrap();
        assert_eq!(
            request(&root, &candidate()).unwrap_err(),
            RequestRefusal::SelectionUnavailable
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn receipt_is_read_only_from_the_private_file_the_host_writes() {
        use std::os::unix::fs::PermissionsExt;
        let root = temporary_root();
        let path = root.join("data/state/package-activation-status.json");
        assert_eq!(receipt(&root), None);
        let write = |value: Value| {
            amaterasu_package::write_json_atomically(&path, &value).unwrap();
        };
        write(json!({"schema":1,"digest":"c".repeat(64),"status":"failed"}));
        assert_eq!(
            receipt(&root),
            Some(Receipt {
                digest: "c".repeat(64),
                status: ReceiptStatus::Failed
            })
        );
        write(json!({"schema":1,"digest":"c".repeat(64),"status":"committed"}));
        assert_eq!(receipt(&root).unwrap().status, ReceiptStatus::Committed);
        write(json!({"schema":1,"digest":"c".repeat(64),"status":"activating"}));
        assert_eq!(receipt(&root).unwrap().status, ReceiptStatus::Activating);

        for unusable in [
            json!({"schema":2,"digest":"c".repeat(64),"status":"failed"}),
            json!({"schema":1,"digest":"C".repeat(64),"status":"failed"}),
            json!({"schema":1,"digest":"c".repeat(63),"status":"failed"}),
            json!({"schema":1,"digest":"c".repeat(64),"status":"approved"}),
            json!({"schema":1,"status":"failed"}),
        ] {
            write(unusable);
            assert_eq!(receipt(&root), None);
        }

        // A file other accounts could have written is not the host's receipt.
        write(json!({"schema":1,"digest":"c".repeat(64),"status":"failed"}));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(receipt(&root), None);
        std::fs::remove_file(&path).unwrap();
        let target = root.join("elsewhere.json");
        std::fs::write(
            &target,
            json!({"schema":1,"digest":"c".repeat(64),"status":"failed"}).to_string(),
        )
        .unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert_eq!(receipt(&root), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
