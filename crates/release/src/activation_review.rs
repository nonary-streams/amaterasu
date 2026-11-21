//! Offline, non-authoritative review of the proposed activation policy.
// Proposed capability only; no host request route is registered by this report.
const HOST_PACKAGE_ACTIVATION_SERVICE: &str = "amaterasu.host-package-activation/1";
const RTX_REPOSITORY_ID: &str = "amaterasu-rtx-hdr";
const RTX_REPOSITORY_URL: &str =
    "https://raw.githubusercontent.com/nonary-streams/amaterasu-rtx-hdr/catalog";
#[cfg(target_os = "linux")]
const CORE_PUBLIC_KEY: &str = "6c399560a1dbe4b646d3be60344fa114d05cdcd7b567a3648958aa1818805b00";
#[cfg(target_os = "linux")]
const CORE_REPOSITORY_ID: &str = "amaterasu";
use amaterasu_package::policy_approval::{PolicyCandidateContext, propose_policy_approval};
use amaterasu_package::{BootstrapConfig, PackageResult, decode_hex, read_bounded, sha256};
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

/// The standard selection for the policy currently installed at `root`.
pub fn default_selection_for_root(
    root: &Path,
) -> PackageResult<crate::profiles::linux_session::ProcessingSelection> {
    let bootstrap: BootstrapConfig =
        serde_json::from_slice(&read_bounded(&root.join("bootstrap.json"), 1024 * 1024)?)
            .map_err(|e| e.to_string())?;
    Ok(crate::profiles::linux_session::default_rtx_hdr_selection_for(&bootstrap.policy))
}

/// Creates only an external report; does not provision the host or grant authority.
pub fn prepare(
    root: &Path,
    expected: &str,
    publisher: &str,
    output: &Path,
    capabilities: &crate::profiles::linux_session::ProcessingSelection,
) -> PackageResult<()> {
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
    let legacy_wiring_converted = crate::profiles::linux_session::compose_rtx_hdr_destination(
        &mut optional.policy,
        &runtime_parent.join("runtime"),
        capabilities,
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
        "kind": "activation_policy_review_v3",
        "selection": capabilities,
        "source_bootstrap_sha256": expected,
        "legacy_rtx_wiring_converted": legacy_wiring_converted,
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

/// Enroll the official Core publisher on a stopped Linux user service.
/// Only signing authority changes. The active set and prior grants stay in place
/// until the normal signed package activation transaction accepts a Core set.
#[cfg(target_os = "linux")]
pub fn provision_core(root: &Path, expected_active: &str, fingerprint: &str) -> PackageResult<()> {
    use amaterasu_package::policy_approval::PolicyAllowlist;
    use amaterasu_package::repository::{RepositoryStore, verify_production_active};
    use amaterasu_package::{
        ActivationState, RootLock, read_json, unix_timestamp, write_json_atomically,
    };
    use std::os::unix::fs::MetadataExt;

    let approval = core_repository(fingerprint)?;
    let uid = validate_user_root(root)?;
    let _lock = RootLock::acquire(root)?;
    if root.join("package-activation-journal.json").exists() {
        return Err("finish the current package activation before provisioning Core".into());
    }
    let selection: ActivationState = read_json(&root.join("selection.json"))?;
    if selection.active.as_deref() != Some(expected_active)
        || selection.pending.is_some()
        || selection.trial.is_some()
    {
        return Err("the active package changed or a transition is in progress".into());
    }
    let bootstrap_path = root.join("bootstrap.json");
    let allowlist_path = root.join("package-activation-policy.json");
    // The atomic writer creates private files owned by the service user.
    // SAFETY: getegid takes no arguments and only reports the current group.
    let gid = unsafe { getegid_for_provisioning() };
    for path in [&bootstrap_path, &allowlist_path] {
        let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.is_file()
            || metadata.uid() != uid
            || metadata.gid() != gid
            || metadata.mode() & 0o7777 != 0o600
        {
            return Err(format!(
                "{} must be a private service-user file",
                path.display()
            ));
        }
    }
    let original = read_bounded(&bootstrap_path, 1024 * 1024)?;
    let mut bootstrap: BootstrapConfig =
        serde_json::from_slice(&original).map_err(|error| error.to_string())?;
    let mut allowlist: PolicyAllowlist = read_json(&allowlist_path)?;
    if bootstrap.development
        || bootstrap.allow_unsigned
        || !bootstrap
            .policy
            .principals
            .get("amaterasu.management")
            .is_some_and(|grants| grants.call.contains(HOST_PACKAGE_ACTIVATION_SERVICE))
    {
        return Err("Core requires a signed production host with package activation".into());
    }
    let active = verify_production_active(root, &bootstrap, &selection, unix_timestamp())?;
    let recorded = selection
        .repository_sets
        .get(expected_active)
        .ok_or("the active package has no recorded supplier")?;
    let store = RepositoryStore::new(root.join("repositories"))?;
    let previous = store.approved(&recorded.repository_id)?;
    if active.signer_fingerprint.as_deref() != Some(recorded.key_fingerprint.as_str())
        || recorded.key_fingerprint != previous.fingerprint
        || (allowlist.publisher_fingerprint != previous.fingerprint
            && allowlist.publisher_fingerprint != fingerprint)
    {
        return Err("the active package and protected publisher authorization differ".into());
    }
    if allowlist.publisher_fingerprint != fingerprint
        && !allowlist_is_fully_applied(&allowlist_path, uid, &previous.fingerprint, &original)?
    {
        return Err(
            "finish the previously approved policy changes before switching publishers".into(),
        );
    }
    if let Some(trusted) = bootstrap.trusted_keys.get(fingerprint)
        && trusted != CORE_PUBLIC_KEY
    {
        return Err("Core fingerprint is already assigned to another key".into());
    }
    enroll_repository(&store, approval)?;
    if !bootstrap.trusted_keys.contains_key(fingerprint) {
        bootstrap
            .trusted_keys
            .insert(fingerprint.into(), CORE_PUBLIC_KEY.into());
        write_json_atomically(&bootstrap_path, &bootstrap)?;
    }
    if allowlist.publisher_fingerprint != fingerprint {
        allowlist.publisher_fingerprint = fingerprint.into();
        write_json_atomically(&allowlist_path, &allowlist)?;
    }
    println!("Authorized the official Core publisher for signed activation.");
    Ok(())
}

#[cfg(target_os = "linux")]
fn core_repository(
    fingerprint: &str,
) -> PackageResult<amaterasu_package::repository::ApprovedRepository> {
    amaterasu_package::repository::ApprovedRepository::approve(
        CORE_REPOSITORY_ID.into(),
        format!(
            "https://raw.githubusercontent.com/nonary-streams/amaterasu-repository/catalog/{}",
            amaterasu_package::current_target()
        ),
        CORE_PUBLIC_KEY.into(),
        fingerprint.into(),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" {
    #[link_name = "getegid"]
    fn getegid_for_provisioning() -> u32;
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
    if report.get("kind").and_then(|v| v.as_str()) != Some("activation_policy_review_v3") {
        return Err("invalid activation review report".into());
    }
    let capabilities: crate::profiles::linux_session::ProcessingSelection = serde_json::from_value(
        report
            .get("selection")
            .cloned()
            .ok_or("review capabilities missing")?,
    )
    .map_err(|_| "invalid review capabilities")?;
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
    // The newest accepted release, or an older set the host was returned to.
    let active = amaterasu_package::repository::verify_production_active(
        root,
        &baseline,
        &selection,
        amaterasu_package::unix_timestamp(),
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
    let legacy_wiring_converted = crate::profiles::linux_session::compose_rtx_hdr_destination(
        &mut optional.policy,
        &runtime_parent.join("runtime"),
        &capabilities,
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
                if legacy_wiring_converted
                    && allowlist_is_fully_applied(&allowlist_path, uid, publisher, &original)?
                {
                    return fs::rename(&allowlist_temp, &allowlist_path).map_err(|e| e.to_string());
                }
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

/// An allowlist from an earlier update can be replaced only when it no longer
/// authorizes anything: the same publisher wrote it and the bootstrap already
/// holds every value it approved. Pending authority is never discarded.
#[cfg(target_os = "linux")]
fn allowlist_is_fully_applied(
    path: &Path,
    uid: u32,
    publisher: &str,
    bootstrap: &[u8],
) -> PackageResult<bool> {
    use amaterasu_package::policy_approval::{PolicyAllowlist, PolicyValue};
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Ok(false);
    }
    let Ok(existing) = serde_json::from_slice::<PolicyAllowlist>(&read_bounded(path, 1024 * 1024)?)
    else {
        return Ok(false);
    };
    if existing.publisher_fingerprint != publisher {
        return Ok(false);
    }
    let bootstrap: serde_json::Value =
        serde_json::from_slice(bootstrap).map_err(|e| e.to_string())?;
    Ok(existing.changes.iter().all(|(pointer, change)| {
        bootstrap
            .pointer(pointer)
            .cloned()
            .map_or(PolicyValue::Absent, PolicyValue::Present)
            == change.after
    }))
}

/// Prepare the fixed RTX HDR policy for the signer of the installed package.
/// This is called by user-service installation and never fetches or loads the
/// optional plugin. It cannot select another publisher or an arbitrary policy.
#[cfg(target_os = "linux")]
pub fn provision_default(root: &Path) -> PackageResult<()> {
    use std::os::unix::fs::DirBuilderExt;
    validate_user_root(root)?;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let (source_digest, publisher, default_selection) = {
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
        let active = amaterasu_package::repository::verify_production_active(
            &root,
            &bootstrap,
            &selection,
            amaterasu_package::unix_timestamp(),
        )?;
        if !active.signed || active.digest != digest || active.manifest.host_context_schema != 1 {
            return Err("active package is not a verified production set".into());
        }
        let publisher = active
            .signer_fingerprint
            .ok_or("active publisher unavailable")?;
        let running_core = selection.repository_sets.get(digest).is_some_and(|origin| {
            origin.repository_id == CORE_REPOSITORY_ID && origin.key_fingerprint == publisher
        });
        // Reinstalling an already active optional package must not attempt to
        // replace the original allowlist with an empty policy delta.
        if running_core
            || active
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
            if running_core {
                let store =
                    amaterasu_package::repository::RepositoryStore::new(root.join("repositories"))?;
                return enroll_repository(&store, core_repository(&publisher)?);
            }
            crate::rtx_repository_publisher::check_composition(
                &active.manifest,
                &active.runtime_config,
            )?;
            return enroll_project_repository(&root, &bootstrap, &publisher);
        }
        let selection =
            crate::profiles::linux_session::default_rtx_hdr_selection_for(&bootstrap.policy);
        (sha256(&bytes), publisher, selection)
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
        prepare(
            &root,
            &source_digest,
            &publisher,
            &report,
            &default_selection,
        )?;
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
    enroll_repository(&RepositoryStore::new(root.join("repositories"))?, expected)
}

#[cfg(target_os = "linux")]
fn enroll_repository(
    store: &amaterasu_package::repository::RepositoryStore,
    expected: amaterasu_package::repository::ApprovedRepository,
) -> PackageResult<()> {
    if let Ok(existing) = store.approved(&expected.id) {
        if existing.location != expected.location
            || existing.fingerprint != expected.fingerprint
            || existing.public_key != expected.public_key
        {
            return Err(format!(
                "existing {} approval differs from the project publisher",
                expected.id
            ));
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

    /// Writes a production bootstrap and a signed active package under `root`,
    /// returning the publisher fingerprint.
    fn install_signed_root(root: &Path, policy: amaterasu_contracts::RuntimeConfig) -> String {
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
        let set = amaterasu_package::package_directory(root, &active_digest).unwrap();
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
        fingerprint
    }

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
        let fingerprint = install_signed_root(&root, policy);
        let before = fs::read(root.join("bootstrap.json")).unwrap();
        let digest = sha256(&before);
        let report = base.join("review.json");
        let capabilities = crate::profiles::linux_session::default_rtx_hdr_selection();
        let lean = crate::profiles::linux_session::ProcessingSelection {
            display_settings: None,
            foreground: None,
            ..crate::profiles::linux_session::default_rtx_hdr_selection()
        };
        let lean_report = base.join("lean-review.json");
        prepare(&root, &digest, &fingerprint, &lean_report, &lean).unwrap();
        let lean_value: serde_json::Value =
            serde_json::from_slice(&fs::read(&lean_report).unwrap()).unwrap();
        assert!(
            lean_value["optional_bootstrap"]["policy"]["principals"]
                .get("amaterasu.foreground-context")
                .is_none()
        );
        assert!(
            lean_value["optional_bootstrap"]["policy"]["principals"]
                .get("amaterasu.display-settings")
                .is_none()
        );
        assert!(prepare(&root, &"0".repeat(64), &fingerprint, &report, &capabilities).is_err());
        assert!(!report.exists());
        assert!(prepare(&root, &digest, &"0".repeat(64), &report, &capabilities).is_err());
        prepare(&root, &digest, &fingerprint, &report, &capabilities).unwrap();
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
            let mut altered_capabilities: serde_json::Value =
                serde_json::from_slice(&report_bytes).unwrap();
            altered_capabilities["selection"]["display_settings"] = serde_json::Value::Null;
            let altered_bytes = serde_json::to_vec(&altered_capabilities).unwrap();
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

    #[cfg(target_os = "linux")]
    #[test]
    fn conversion_replaces_only_a_fully_applied_allowlist() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!(
            "amaterasu-conversion-allowlist-{}",
            random_identifier().unwrap()
        ));
        let root = base.join("root");
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let options = crate::profiles::linux_session::options::LinuxSessionOptions {
            frame_helper: Some("/usr/bin/amaterasu-vkms-frame-helper".into()),
            rtx_hdr_runtime_directory: Some(root.join("data/state/rtx-hdr/runtime")),
            ..Default::default()
        };
        let mut policy =
            crate::profiles::linux_session::linux_session_policy(&root, &options).unwrap();
        policy.processing_compositions.clear();
        let video = policy
            .settings
            .get_mut("amaterasu.linux-session-video")
            .unwrap()
            .as_object_mut()
            .unwrap();
        video.insert("gpu_processor_service".into(), json!("amaterasu.rtx-hdr/1"));
        video.insert("gpu_processor_opt_in".into(), json!(true));
        let fingerprint = install_signed_root(&root, policy);
        let bootstrap_path = root.join("bootstrap.json");
        fs::set_permissions(&bootstrap_path, fs::Permissions::from_mode(0o600)).unwrap();
        let source = fs::read(&bootstrap_path).unwrap();
        let report = base.join("review.json");
        let selection = crate::profiles::linux_session::default_rtx_hdr_selection();
        prepare(&root, &sha256(&source), &fingerprint, &report, &selection).unwrap();
        let report_bytes = fs::read(&report).unwrap();
        let reviewed: serde_json::Value = serde_json::from_slice(&report_bytes).unwrap();
        assert_eq!(reviewed["legacy_rtx_wiring_converted"], true);
        let report_digest = sha256(&report_bytes);

        let allowlist_path = root.join("package-activation-policy.json");
        let write_allowlist = |publisher: &str, after: serde_json::Value| {
            let bytes = serde_json::to_vec(&json!({
                "publisher_fingerprint": publisher,
                "changes": {"/policy/settings/amaterasu.stream-session/enabled": {
                    "before": {"kind": "absent"},
                    "after": after,
                }},
            }))
            .unwrap();
            fs::write(&allowlist_path, &bytes).unwrap();
            fs::set_permissions(&allowlist_path, fs::Permissions::from_mode(0o600)).unwrap();
            bytes
        };
        let current_value = serde_json::from_slice::<serde_json::Value>(&source).unwrap()["policy"]
            ["settings"]["amaterasu.stream-session"]["enabled"]
            .clone();
        let applied = json!({"kind": "present", "value": current_value});

        // An authorization that has not been consumed yet must survive.
        let pending = write_allowlist(&fingerprint, json!({"kind": "present", "value": "new"}));
        assert!(provision(&root, &report, &report_digest).is_err());
        assert_eq!(fs::read(&allowlist_path).unwrap(), pending);

        // So must one issued for a different publisher.
        let foreign = write_allowlist(&"f".repeat(64), applied.clone());
        assert!(provision(&root, &report, &report_digest).is_err());
        assert_eq!(fs::read(&allowlist_path).unwrap(), foreign);
        assert_eq!(fs::read(&bootstrap_path).unwrap(), source);

        write_allowlist(&fingerprint, applied);
        provision(&root, &report, &report_digest).unwrap();
        let installed: serde_json::Value =
            serde_json::from_slice(&fs::read(&allowlist_path).unwrap()).unwrap();
        assert_eq!(installed, reviewed["proposed_allowlist"]);
        fs::remove_dir_all(base).unwrap();
    }

    /// Runs the production review against a copy of a real bootstrap, with its
    /// installation path rewritten so the original root is never touched.
    /// `AMATERASU_MIGRATED_REPORT` is written for the provider parser tests.
    #[test]
    #[ignore = "needs AMATERASU_INSTALLED_BOOTSTRAP and AMATERASU_MIGRATED_REPORT"]
    fn installed_bootstrap_converts_through_the_reviewed_flow() {
        use amaterasu_package::policy_approval::{PolicyAllowlist, candidate_from_allowlist};
        let source = std::env::var("AMATERASU_INSTALLED_BOOTSTRAP").unwrap();
        let output = std::env::var("AMATERASU_MIGRATED_REPORT").unwrap();
        let text = fs::read_to_string(source).unwrap();
        let installed: serde_json::Value = serde_json::from_str(&text).unwrap();
        let parent = installed
            .pointer("/policy/settings/amaterasu.management/rtx_hdr_runtime_parent")
            .and_then(|value| value.as_str())
            .unwrap();
        let installed_root = parent.strip_suffix("/data/state/rtx-hdr").unwrap();
        let base = std::env::temp_dir().join(format!(
            "amaterasu-installed-review-{}",
            random_identifier().unwrap()
        ));
        let root = base.join("root");
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let copy = text.replace(installed_root, root.to_str().unwrap());
        fs::write(root.join("bootstrap.json"), &copy).unwrap();
        let bootstrap: BootstrapConfig = serde_json::from_str(&copy).unwrap();
        let publisher = bootstrap
            .trusted_keys
            .values()
            .map(|key| sha256(&decode_hex::<32>(key).unwrap()))
            .next()
            .unwrap();
        let selection = default_selection_for_root(&root).unwrap();
        assert!(selection.foreground.is_some());
        prepare(
            &root,
            &sha256(copy.as_bytes()),
            &publisher,
            Path::new(&output),
            &selection,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("bootstrap.json")).unwrap(),
            copy
        );
        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
        assert_eq!(report["legacy_rtx_wiring_converted"], true);
        let settings = |id: &str| {
            report["optional_bootstrap"]["policy"]["settings"][id]
                .as_object()
                .unwrap()
                .clone()
        };
        let video = settings("amaterasu.linux-session-video");
        assert!(!video.contains_key("gpu_processor_service"));
        assert_eq!(video["require_frame_lease"], true);
        let capture = settings("amaterasu.capture-vkms");
        assert_eq!(capture["gpu_snapshot"], true);
        assert_eq!(capture["require_lease"], true);
        let session = settings("amaterasu.stream-session");
        assert!(!session.contains_key("processing_plan"));

        // The host rebuilds the destination from the allowlist, so that exact
        // reconstruction has to reach the reviewed result.
        let baseline = serde_json::to_vec_pretty(&report["baseline_bootstrap"]).unwrap();
        let allowlist: PolicyAllowlist =
            serde_json::from_value(report["proposed_allowlist"].clone()).unwrap();
        let placeholder = "0".repeat(64);
        let context = PolicyCandidateContext {
            selection_revision: &placeholder,
            publisher_fingerprint: &publisher,
            manifest_sha256: &placeholder,
        };
        let (candidate, _) = candidate_from_allowlist(&allowlist, &context, &baseline).unwrap();
        let candidate: serde_json::Value = serde_json::from_slice(&candidate).unwrap();
        assert_eq!(candidate, report["optional_bootstrap"]);
        serde_json::from_value::<BootstrapConfig>(candidate).unwrap();
        fs::remove_dir_all(base).unwrap();
    }
}
