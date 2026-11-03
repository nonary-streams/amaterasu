//! Offline, non-authoritative review of the proposed activation policy.
// Proposed capability only; no host request route is registered by this report.
const HOST_PACKAGE_ACTIVATION_SERVICE: &str = "amaterasu.host-package-activation/1";
const RTX_REPOSITORY_ID: &str = "amaterasu-rtx-hdr";
const RTX_REPOSITORY_URL: &str =
    "https://raw.githubusercontent.com/nonary-streams/amaterasu-rtx-hdr/catalog";
use amaterasu_package::policy_approval::{PolicyCandidateContext, propose_policy_approval};
use amaterasu_package::{BootstrapConfig, PackageResult, decode_hex, read_bounded, sha256};
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

/// Creates only an external report; does not provision the host or grant authority.
pub fn prepare(root: &Path, expected: &str, publisher: &str, output: &Path) -> PackageResult<()> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let parent = output
        .parent()
        .ok_or("review output needs a parent")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if parent.starts_with(&root) {
        return Err("review output must be outside the installation root".into());
    }
    let path = root.join("bootstrap.json");
    let source = read_bounded(&path, 1024 * 1024)?;
    if sha256(&source) != expected {
        return Err("bootstrap changed; retry review".into());
    }
    let mut baseline: BootstrapConfig =
        serde_json::from_slice(&source).map_err(|e| e.to_string())?;
    if baseline.development || baseline.allow_unsigned || baseline.trusted_keys.is_empty() {
        return Err("review requires a signed production bootstrap".into());
    }
    if !baseline
        .trusted_keys
        .values()
        .any(|key| decode_hex::<32>(key).is_ok_and(|value| sha256(&value) == publisher))
    {
        return Err("publisher is not trusted by the production bootstrap".into());
    }
    baseline
        .policy
        .principals
        .get_mut("amaterasu.management")
        .ok_or("production bootstrap has no management principal")?
        .call
        .insert(HOST_PACKAGE_ACTIVATION_SERVICE.into());
    let baseline_bytes = serde_json::to_vec_pretty(&baseline).map_err(|e| e.to_string())?;
    let mut optional = baseline.clone();
    let runtime_parent = root.join("data/state/rtx-hdr");
    let configured = optional
        .policy
        .settings
        .get("amaterasu.management")
        .and_then(|v| v.get("rtx_hdr_runtime_parent"))
        .and_then(|v| v.as_str())
        .ok_or("trusted RTX HDR runtime parent is unavailable")?;
    if Path::new(configured) != runtime_parent {
        return Err("RTX HDR runtime parent differs from host state".into());
    }
    crate::profiles::linux_session::enable_rtx_hdr(
        &mut optional.policy,
        &runtime_parent.join("runtime"),
    )?;
    let optional_bytes = serde_json::to_vec_pretty(&optional).map_err(|e| e.to_string())?;
    let placeholder = "0".repeat(64);
    let context = PolicyCandidateContext {
        selection_revision: &placeholder,
        publisher_fingerprint: publisher,
        manifest_sha256: &placeholder,
    };
    let proposal = propose_policy_approval(&baseline_bytes, &optional_bytes, &context)?;
    let report = serde_json::to_vec_pretty(&json!({
        "kind": "activation_policy_review_v1",
        "source_bootstrap_sha256": expected,
        "note": "Review only; this report does not authorize or modify host policy.",
        "baseline_bootstrap": baseline,
        "optional_bootstrap": optional,
        "proposed_allowlist": {"publisher_fingerprint": publisher, "changes": proposal.changes},
    }))
    .map_err(|e| e.to_string())?;
    if sha256(&read_bounded(&path, 1024 * 1024)?) != expected {
        return Err("bootstrap changed during review".into());
    }
    let destination = parent.join(output.file_name().ok_or("review output needs a name")?);
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&destination).map_err(|e| e.to_string())?;
    if let Err(error) = file.write_all(&report).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&destination);
        return Err(error.to_string());
    }
    println!(
        "Review-only proposal {} (SHA-256 {}).",
        destination.display(),
        sha256(&report)
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_user_root(root: &Path) -> PackageResult<u32> {
    use std::os::unix::fs::MetadataExt;
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: geteuid takes no arguments and has no memory ownership requirements.
    let uid = unsafe { geteuid() };
    if uid == 0 {
        return Err("provision as the service user, not root".into());
    }
    if !root.is_absolute()
        || root.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err("installation root must be absolute and normalized".into());
    }
    let mut part = std::path::PathBuf::new();
    for component in root.components() {
        part.push(component.as_os_str());
        if fs::symlink_metadata(&part)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("installation root cannot contain symlinks".into());
        }
    }
    let meta = fs::metadata(root).map_err(|e| e.to_string())?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o022 != 0 {
        return Err("installation root must be user-owned and private".into());
    }
    let lock_path = root.join("host.lock");
    match fs::symlink_metadata(&lock_path) {
        Ok(metadata)
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.uid() != uid
                || metadata.mode() & 0o022 != 0 =>
        {
            return Err("host lock must be a private user-owned file".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    Ok(uid)
}

/// Apply a previously reviewed authorization to a stopped Linux user service.
/// The report digest is supplied by the administrator after review. This does
/// not select or load a package; the host still verifies every trial.
#[cfg(target_os = "linux")]
pub fn provision(root: &Path, report_path: &Path, expected_report: &str) -> PackageResult<()> {
    use amaterasu_package::{RootLock, random_identifier};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let uid = validate_user_root(root)?;
    let _lock = RootLock::acquire(root)?;
    let bytes = read_bounded(report_path, 1024 * 1024)?;
    if sha256(&bytes) != expected_report {
        return Err("review report changed".into());
    }
    let report: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if report.get("kind").and_then(|v| v.as_str()) != Some("activation_policy_review_v1") {
        return Err("invalid activation review report".into());
    }
    let path = root.join("bootstrap.json");
    let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.uid() != uid
        || meta.mode() & 0o022 != 0
    {
        return Err("bootstrap must be a private user-owned file".into());
    }
    let original = read_bounded(&path, 1024 * 1024)?;
    if report
        .get("source_bootstrap_sha256")
        .and_then(|v| v.as_str())
        != Some(sha256(&original).as_str())
    {
        return Err("bootstrap changed since review".into());
    }
    let mut baseline: BootstrapConfig =
        serde_json::from_slice(&original).map_err(|e| e.to_string())?;
    if baseline.development || baseline.allow_unsigned || baseline.trusted_keys.is_empty() {
        return Err("activation requires a signed production bootstrap".into());
    }
    let selection: amaterasu_package::ActivationState =
        amaterasu_package::read_json(&root.join("selection.json"))?;
    if selection.pending.is_some() || selection.trial.is_some() {
        return Err("host package transition is already in progress".into());
    }
    let active_digest = selection
        .active
        .as_deref()
        .ok_or("active package is unavailable")?;
    let active = amaterasu_package::verify_package(
        &amaterasu_package::package_directory(root, active_digest)?,
        &baseline,
        amaterasu_package::VerificationPolicy {
            minimum_sequence: selection.highest_sequence,
            allow_historical: false,
            now: amaterasu_package::unix_timestamp(),
        },
    )?;
    if !active.signed
        || active.digest != active_digest
        || active.manifest.host_context_schema != 1
        || !active.manifest.components.iter().any(|component| {
            component.id == "amaterasu.management"
                && component
                    .requested
                    .call
                    .contains(HOST_PACKAGE_ACTIVATION_SERVICE)
        })
    {
        return Err(
            "install a signed baseline that supports package activation before provisioning".into(),
        );
    }
    let publisher = report
        .pointer("/proposed_allowlist/publisher_fingerprint")
        .and_then(|v| v.as_str())
        .ok_or("review publisher missing")?;
    if !baseline
        .trusted_keys
        .values()
        .any(|key| decode_hex::<32>(key).is_ok_and(|value| sha256(&value) == publisher))
    {
        return Err("review publisher is not trusted".into());
    }
    baseline
        .policy
        .principals
        .get_mut("amaterasu.management")
        .ok_or("management principal missing")?
        .call
        .insert(HOST_PACKAGE_ACTIVATION_SERVICE.into());
    if serde_json::to_value(&baseline).map_err(|e| e.to_string())? != report["baseline_bootstrap"] {
        return Err("reviewed baseline does not match current host".into());
    }
    let mut optional = baseline.clone();
    let runtime_parent = root.join("data/state/rtx-hdr");
    if optional
        .policy
        .settings
        .get("amaterasu.management")
        .and_then(|v| v.get("rtx_hdr_runtime_parent"))
        .and_then(|v| v.as_str())
        != runtime_parent.to_str()
    {
        return Err("runtime path differs from trusted host state".into());
    }
    crate::profiles::linux_session::enable_rtx_hdr(
        &mut optional.policy,
        &runtime_parent.join("runtime"),
    )?;
    if serde_json::to_value(&optional).map_err(|e| e.to_string())? != report["optional_bootstrap"] {
        return Err("reviewed optional policy does not match host policy".into());
    }
    let baseline_bytes = serde_json::to_vec_pretty(&baseline).map_err(|e| e.to_string())?;
    let optional_bytes = serde_json::to_vec_pretty(&optional).map_err(|e| e.to_string())?;
    let placeholder = "0".repeat(64);
    let context = PolicyCandidateContext {
        selection_revision: &placeholder,
        publisher_fingerprint: publisher,
        manifest_sha256: &placeholder,
    };
    let proposal = propose_policy_approval(&baseline_bytes, &optional_bytes, &context)?;
    let allowlist = json!({"publisher_fingerprint": publisher, "changes": proposal.changes});
    if allowlist != report["proposed_allowlist"] {
        return Err("reviewed allowlist does not match policy".into());
    }
    let allowlist_bytes = serde_json::to_vec_pretty(&allowlist).map_err(|e| e.to_string())?;
    let allowlist_path = root.join("package-activation-policy.json");
    let allowlist_temp = root.join(format!(".activation-policy-{}", random_identifier()?));
    let publish = (|| -> PackageResult<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&allowlist_temp)
            .map_err(|e| e.to_string())?;
        file.write_all(&allowlist_bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        match fs::hard_link(&allowlist_temp, &allowlist_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let m = fs::symlink_metadata(&allowlist_path).map_err(|e| e.to_string())?;
                if !m.is_file()
                    || m.file_type().is_symlink()
                    || m.uid() != uid
                    || m.mode() & 0o077 != 0
                    || read_bounded(&allowlist_path, 1024 * 1024)? != allowlist_bytes
                {
                    return Err("existing activation policy differs from review".into());
                }
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    })();
    let _ = fs::remove_file(&allowlist_temp);
    publish?;
    fs::File::open(root)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    if read_bounded(&path, 1024 * 1024)? != original {
        return Err("bootstrap changed during provisioning".into());
    }
    let temporary = root.join(format!(".activation-bootstrap-{}", random_identifier()?));
    let result = (|| -> PackageResult<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        let new_meta = file.metadata().map_err(|e| e.to_string())?;
        if new_meta.uid() != meta.uid() || new_meta.gid() != meta.gid() {
            return Err("cannot preserve bootstrap ownership".into());
        }
        file.set_permissions(fs::Permissions::from_mode(meta.mode() & 0o7777))
            .map_err(|e| e.to_string())?;
        file.write_all(&baseline_bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
        fs::File::open(root)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Prepare the fixed RTX HDR policy for the signer of the installed package.
/// This is called by user-service installation and never fetches or loads the
/// optional plugin. It cannot select another publisher or an arbitrary policy.
#[cfg(target_os = "linux")]
pub fn provision_default(root: &Path) -> PackageResult<()> {
    use std::os::unix::fs::DirBuilderExt;
    validate_user_root(root)?;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let (source_digest, publisher) = {
        let _lock = amaterasu_package::RootLock::acquire(&root)?;
        let bytes = read_bounded(&root.join("bootstrap.json"), 1024 * 1024)?;
        let bootstrap: BootstrapConfig =
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let selection: amaterasu_package::ActivationState =
            amaterasu_package::read_json(&root.join("selection.json"))?;
        if bootstrap.development
            || bootstrap.allow_unsigned
            || selection.pending.is_some()
            || selection.trial.is_some()
        {
            return Err("automatic authorization requires a settled signed production host".into());
        }
        let digest = selection
            .active
            .as_deref()
            .ok_or("active package unavailable")?;
        let active = amaterasu_package::verify_package(
            &amaterasu_package::package_directory(&root, digest)?,
            &bootstrap,
            amaterasu_package::VerificationPolicy {
                minimum_sequence: selection.highest_sequence,
                allow_historical: false,
                now: amaterasu_package::unix_timestamp(),
            },
        )?;
        if !active.signed || active.digest != digest || active.manifest.host_context_schema != 1 {
            return Err("active package is not a verified production set".into());
        }
        let publisher = active
            .signer_fingerprint
            .ok_or("active publisher unavailable")?;
        // Reinstalling an already active optional package must not attempt to
        // replace the original allowlist with an empty policy delta.
        if active
            .manifest
            .components
            .iter()
            .any(|component| component.id == "amaterasu.rtx-hdr")
        {
            use std::os::unix::fs::MetadataExt;
            let policy_path = root.join("package-activation-policy.json");
            let metadata = fs::symlink_metadata(&policy_path).map_err(|e| e.to_string())?;
            let uid = validate_user_root(&root)?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.uid() != uid
                || metadata.mode() & 0o077 != 0
            {
                return Err("existing activation authority is not private".into());
            }
            let allowlist: amaterasu_package::policy_approval::PolicyAllowlist =
                serde_json::from_slice(&read_bounded(&policy_path, 1024 * 1024)?)
                    .map_err(|e| e.to_string())?;
            if allowlist.publisher_fingerprint != publisher
                || !bootstrap
                    .policy
                    .principals
                    .get("amaterasu.management")
                    .is_some_and(|grants| grants.call.contains(HOST_PACKAGE_ACTIVATION_SERVICE))
            {
                return Err(
                    "existing activation authority differs from the active publisher".into(),
                );
            }
            let mut expected = bootstrap.policy.clone();
            crate::profiles::linux_session::enable_rtx_hdr(
                &mut expected,
                &root.join("data/state/rtx-hdr/runtime"),
            )?;
            if serde_json::to_value(&expected).map_err(|e| e.to_string())?
                != serde_json::to_value(&bootstrap.policy).map_err(|e| e.to_string())?
            {
                return Err("active RTX HDR policy differs from the installed composition".into());
            }
            return enroll_project_repository(&root, &bootstrap, &publisher);
        }
        (sha256(&bytes), publisher)
    };
    // A private, exclusive temporary directory keeps the review proposal out of
    // the installation root and protects it until its digest is verified.
    let directory = std::env::temp_dir().join(format!(
        "amaterasu-activation-review-{}",
        amaterasu_package::random_identifier()?
    ));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .map_err(|e| e.to_string())?;
    let report = directory.join("policy.json");
    let result = (|| {
        prepare(&root, &source_digest, &publisher, &report)?;
        let digest = sha256(&read_bounded(&report, 1024 * 1024)?);
        provision(&root, &report, &digest)
    })();
    let _ = fs::remove_file(&report);
    let _ = fs::remove_dir(&directory);
    result?;
    let _lock = amaterasu_package::RootLock::acquire(&root)?;
    let bootstrap: BootstrapConfig = amaterasu_package::read_json(&root.join("bootstrap.json"))?;
    let allowlist: amaterasu_package::policy_approval::PolicyAllowlist =
        amaterasu_package::read_json(&root.join("package-activation-policy.json"))?;
    if allowlist.publisher_fingerprint != publisher {
        return Err("activation publisher changed while enrolling repository".into());
    }
    enroll_project_repository(&root, &bootstrap, &publisher)
}

/// Called while holding the host lock, after verifying the active publisher.
#[cfg(target_os = "linux")]
fn enroll_project_repository(
    root: &Path,
    bootstrap: &BootstrapConfig,
    publisher: &str,
) -> PackageResult<()> {
    use amaterasu_package::repository::{ApprovedRepository, RepositoryStore};
    let key = bootstrap
        .trusted_keys
        .values()
        .find(|key| decode_hex::<32>(key).is_ok_and(|value| sha256(&value) == publisher))
        .ok_or("active publisher is no longer trusted")?;
    let expected = ApprovedRepository::approve(
        RTX_REPOSITORY_ID.into(),
        RTX_REPOSITORY_URL.into(),
        key.clone(),
        publisher.into(),
    )?;
    let store = RepositoryStore::new(root.join("repositories"))?;
    if let Ok(existing) = store.approved(RTX_REPOSITORY_ID) {
        if existing.location != expected.location
            || existing.fingerprint != expected.fingerprint
            || existing.public_key != expected.public_key
        {
            return Err(
                "existing RTX HDR repository approval differs from the project publisher".into(),
            );
        }
        return Ok(());
    }
    // A revoked or conflicting existing ID must not be silently replaced.
    store.approve(expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_package::{encode_hex, random_identifier, write_json_atomically};

    #[test]
    fn review_reports_exact_proposed_grant_without_modifying_host() {
        let base = std::env::temp_dir().join(format!(
            "amaterasu-activation-review-{}",
            random_identifier().unwrap()
        ));
        let root = base.join("root");
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let options = crate::profiles::linux_session::options::LinuxSessionOptions {
            frame_helper: Some("/usr/bin/amaterasu-vkms-frame-helper".into()),
            ..Default::default()
        };
        let policy = crate::profiles::linux_session::linux_session_policy(&root, &options).unwrap();
        use ed25519_dalek::{Signer, SigningKey};
        let signer = SigningKey::from_bytes(&[11u8; 32]);
        let key = *signer.verifying_key().as_bytes();
        let fingerprint = sha256(&key);
        let bootstrap = BootstrapConfig {
            development: false,
            allow_unsigned: false,
            trusted_keys: [(fingerprint.clone(), encode_hex(&key))].into(),
            policy,
        };
        write_json_atomically(&root.join("bootstrap.json"), &bootstrap).unwrap();
        let before = fs::read(root.join("bootstrap.json")).unwrap();
        let artifact = b"synthetic signed component";
        let make_component =
            |id: &str, file: &str, runtime, requested| amaterasu_package::ComponentManifest {
                id: id.into(),
                version: "1".into(),
                file: file.into(),
                sha256: sha256(artifact),
                size: artifact.len() as u64,
                abi_major: amaterasu_abi::ABI_MAJOR,
                runtime,
                requires: std::collections::BTreeMap::new(),
                requested,
            };
        let grants = amaterasu_contracts::Grants {
            call: [HOST_PACKAGE_ACTIVATION_SERVICE.to_owned()].into(),
            ..Default::default()
        };
        let manifest = amaterasu_package::ReleaseManifest {
            schema: 1,
            host_context_schema: 1,
            project: "amaterasu".into(),
            sequence: 1,
            expires_unix: amaterasu_package::unix_timestamp() + 3600,
            target: amaterasu_package::current_target(),
            components: vec![
                make_component("amaterasu.runtime", "runtime.bin", true, Default::default()),
                make_component("amaterasu.management", "management.bin", false, grants),
            ],
            privileged_requests: Vec::new(),
        };
        let raw = serde_json::to_vec(&manifest).unwrap();
        let active_digest = sha256(&raw);
        let set = amaterasu_package::package_directory(&root, &active_digest).unwrap();
        fs::create_dir_all(&set).unwrap();
        fs::write(set.join("runtime.bin"), artifact).unwrap();
        fs::write(set.join("management.bin"), artifact).unwrap();
        fs::write(set.join("manifest.json"), &raw).unwrap();
        write_json_atomically(
            &set.join("manifest.sig"),
            &amaterasu_package::ReleaseSignature {
                key_id: fingerprint.clone(),
                signature: encode_hex(&signer.sign(&raw).to_bytes()),
            },
        )
        .unwrap();
        write_json_atomically(
            &root.join("selection.json"),
            &amaterasu_package::ActivationState {
                active: Some(active_digest),
                highest_sequence: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let digest = sha256(&before);
        let report = base.join("review.json");
        assert!(prepare(&root, &"0".repeat(64), &fingerprint, &report).is_err());
        assert!(!report.exists());
        assert!(prepare(&root, &digest, &"0".repeat(64), &report).is_err());
        prepare(&root, &digest, &fingerprint, &report).unwrap();
        assert_eq!(fs::read(root.join("bootstrap.json")).unwrap(), before);
        assert!(!root.join("package-activation-policy.json").exists());
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
        assert!(
            value["baseline_bootstrap"]["policy"]["principals"]["amaterasu.management"]["call"]
                .as_array()
                .unwrap()
                .contains(&json!(HOST_PACKAGE_ACTIVATION_SERVICE))
        );
        assert_eq!(
            value["proposed_allowlist"]["publisher_fingerprint"],
            fingerprint
        );
        assert!(
            !value["proposed_allowlist"]["changes"]
                .as_object()
                .unwrap()
                .is_empty()
        );
        #[cfg(target_os = "linux")]
        {
            let report_bytes = fs::read(&report).unwrap();
            let report_digest = sha256(&report_bytes);
            assert!(provision(&root, &report, &"0".repeat(64)).is_err());
            let lock = amaterasu_package::RootLock::acquire(&root).unwrap();
            assert!(provision(&root, &report, &report_digest).is_err());
            drop(lock);
            let mut altered: serde_json::Value = serde_json::from_slice(&report_bytes).unwrap();
            altered["optional_bootstrap"]["policy"]["settings"]["extra"] = json!({"forged":true});
            let altered_path = base.join("altered.json");
            let altered_bytes = serde_json::to_vec(&altered).unwrap();
            fs::write(&altered_path, &altered_bytes).unwrap();
            assert!(provision(&root, &altered_path, &sha256(&altered_bytes)).is_err());
            assert_eq!(fs::read(root.join("bootstrap.json")).unwrap(), before);
            let policy_path = root.join("package-activation-policy.json");
            assert!(!policy_path.exists());
            // A different pre-existing policy cannot be overwritten.
            fs::write(&policy_path, b"wrong").unwrap();
            assert!(provision(&root, &report, &report_digest).is_err());
            assert_eq!(fs::read(&policy_path).unwrap(), b"wrong");
            fs::remove_file(&policy_path).unwrap();
            // Recover if publication of the reviewed policy succeeded before
            // the bootstrap replacement was interrupted.
            use std::os::unix::fs::PermissionsExt;
            fs::write(
                &policy_path,
                serde_json::to_vec_pretty(&value["proposed_allowlist"]).unwrap(),
            )
            .unwrap();
            fs::set_permissions(&policy_path, fs::Permissions::from_mode(0o600)).unwrap();
            provision(&root, &report, &report_digest).unwrap();
            let installed: serde_json::Value =
                serde_json::from_slice(&fs::read(root.join("bootstrap.json")).unwrap()).unwrap();
            assert_eq!(installed, value["baseline_bootstrap"]);
            let allowlist: serde_json::Value = serde_json::from_slice(
                &fs::read(root.join("package-activation-policy.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(allowlist, value["proposed_allowlist"]);
            assert!(provision(&root, &report, &report_digest).is_err());
            // User-service installation derives the same fixed authority from
            // the verified active signer, without bundling the optional files.
            fs::write(root.join("bootstrap.json"), &before).unwrap();
            fs::remove_file(root.join("package-activation-policy.json")).unwrap();
            provision_default(&root).unwrap();
            let installed: serde_json::Value =
                serde_json::from_slice(&fs::read(root.join("bootstrap.json")).unwrap()).unwrap();
            assert_eq!(installed, value["baseline_bootstrap"]);
            let authorized: serde_json::Value = serde_json::from_slice(
                &fs::read(root.join("package-activation-policy.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(authorized, value["proposed_allowlist"]);
            provision_default(&root).unwrap();
            let store =
                amaterasu_package::repository::RepositoryStore::new(root.join("repositories"))
                    .unwrap();
            let enrolled = store.approved(RTX_REPOSITORY_ID).unwrap();
            assert_eq!(enrolled.fingerprint, fingerprint);
            assert_eq!(enrolled.location, RTX_REPOSITORY_URL);
            store.revoke(RTX_REPOSITORY_ID).unwrap();
            assert!(provision_default(&root).is_err());
        }
        fs::remove_dir_all(base).unwrap();
    }
}
