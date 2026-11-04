//! Host-owned preparation for an approved production package trial.
use amaterasu_contracts::HostPackageActivation;
use amaterasu_package::policy_approval::{
    PolicyAllowlist, PolicyCandidateContext, candidate_from_allowlist,
};
use amaterasu_package::repository::{
    ProductionTrialPolicy, RepositoryStore, stage_verified_production_trial,
};
use amaterasu_package::{
    ActivationState, BootstrapConfig, PackageResult, VerifiedPackage, read_bounded, sha256,
    unix_timestamp,
};
use std::{fs, os::unix::fs::MetadataExt, path::Path};

fn read_allowlist(root: &Path) -> PackageResult<PolicyAllowlist> {
    let policy_path = root.join("package-activation-policy.json");
    let metadata = fs::symlink_metadata(&policy_path).map_err(|e| e.to_string())?;
    // SAFETY: geteuid takes no pointers and only reports the current uid.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != uid
        || metadata.mode() & 0o077 != 0
    {
        return Err("host activation policy is not a private user-owned file".into());
    }
    let allowlist: PolicyAllowlist =
        serde_json::from_slice(&read_bounded(&policy_path, 1024 * 1024)?)
            .map_err(|e| e.to_string())?;
    Ok(allowlist)
}

/// The root lock must remain held for this operation. Trial execution is
/// supplied by the host; no browser or downloaded component may select it.
pub fn prepare_trial<T>(
    root: &Path,
    request: &HostPackageActivation,
    trial: impl FnOnce(&Path, &VerifiedPackage, &BootstrapConfig) -> PackageResult<T>,
) -> PackageResult<(T, Vec<u8>)> {
    let allowlist = read_allowlist(root)?;
    let original = read_bounded(&root.join("bootstrap.json"), 1024 * 1024)?;
    let bootstrap: BootstrapConfig =
        serde_json::from_slice(&original).map_err(|e| e.to_string())?;
    if !bootstrap
        .policy
        .principals
        .get("amaterasu.management")
        .is_some_and(|grants| {
            grants
                .call
                .contains(amaterasu_contracts::HOST_PACKAGE_ACTIVATION_SERVICE)
        })
    {
        return Err("management has not been authorized for package activation".into());
    }
    let selection_bytes = read_bounded(&root.join("selection.json"), 1024 * 1024)?;
    let selection: ActivationState =
        serde_json::from_slice(&selection_bytes).map_err(|e| e.to_string())?;
    if sha256(&selection_bytes) != request.expected_revision
        || selection.active.as_deref() != Some(request.expected_active.as_str())
    {
        return Err("package selection changed".into());
    }
    let context = PolicyCandidateContext {
        selection_revision: &request.expected_revision,
        publisher_fingerprint: &allowlist.publisher_fingerprint,
        manifest_sha256: &request.manifest_digest,
    };
    let (proposed, approval) = candidate_from_allowlist(&allowlist, &context, &original)?;
    let store = RepositoryStore::new(root.join("repositories"))?;
    let outcome = stage_verified_production_trial(
        root,
        &bootstrap,
        &store,
        &request.repository_id,
        &request.catalog_digest,
        &request.manifest_digest,
        &request.expected_active,
        &request.expected_revision,
        unix_timestamp(),
        ProductionTrialPolicy {
            current_bytes: &original,
            candidate_bytes: &proposed,
            allowlist: &allowlist,
            approval: &approval,
        },
        trial,
    )
    .map_err(|e| format!("{e:?}"))?;
    Ok((outcome, proposed))
}

/// Run a disposable trial against a stopped Linux user service. A successful
/// probe does not change the active selection or install a service unit.
pub fn probe(root: &Path, request: &HostPackageActivation) -> PackageResult<()> {
    let root = crate::service::validate_user_service_root(root)?;
    let _lock = amaterasu_package::RootLock::acquire(&root)?;
    let state_root = crate::service::validate_user_state_root(&root.join("data/state"))?;
    prepare_trial(&root, request, |path, verified, bootstrap| {
        let mut package = amaterasu_package::verify_package(
            path,
            bootstrap,
            amaterasu_package::VerificationPolicy {
                minimum_sequence: verified.manifest.sequence,
                allow_historical: false,
                now: unix_timestamp(),
            },
        )?;
        if package.digest != verified.digest
            || package.signer_fingerprint != verified.signer_fingerprint
        {
            return Err("trial package changed before loading".into());
        }
        let logging = crate::logging_config::HostLoggingConfig::from_global(
            bootstrap.policy.settings.get("global"),
            &state_root,
        )?;
        logging.inject(&mut package.runtime_config)?;
        let mut loaded = crate::loading::LoadedComponents::load(
            &root,
            path,
            &package,
            &state_root,
            logging.start_product_logger(),
            &mut |_| Ok(()),
        )?;
        loaded.tick()?;
        loaded.health()?;
        loaded.quiesce(std::time::Duration::from_secs(20))?;
        drop(loaded);
        Ok(())
    })?;
    Ok(())
}

/// Build the next selection after a successful trial. Callers must persist it
/// only in the same recoverable transition as the bootstrap and service pin.
fn next_selection(
    current: &ActivationState,
    request: &HostPackageActivation,
    fingerprint: &str,
    sequence: u64,
) -> PackageResult<ActivationState> {
    if current.active.as_deref() != Some(request.expected_active.as_str())
        || current.pending.is_some()
        || current.trial.is_some()
        || sequence <= current.highest_sequence
        || request.manifest_digest == request.expected_active
    {
        return Err("package selection cannot advance from this state".into());
    }
    if let Some(floor) = current.repository_floors.get(fingerprint)
        && sequence <= floor.sequence
    {
        return Err("package does not advance the publisher release floor".into());
    }
    let mut next = current.clone();
    next.previous = next.active.replace(request.manifest_digest.clone());
    next.highest_sequence = sequence;
    next.healthy_sets.insert(request.manifest_digest.clone());
    next.repository_sets.insert(
        request.manifest_digest.clone(),
        amaterasu_package::RepositorySet {
            repository_id: request.repository_id.clone(),
            key_fingerprint: fingerprint.into(),
        },
    );
    next.repository_floors.insert(
        fingerprint.into(),
        amaterasu_package::NativeReleaseFloor {
            sequence,
            digest: request.manifest_digest.clone(),
        },
    );
    Ok(next)
}

/// Reconstruct the authorized transition using the independently provisioned
/// allowlist and the signed sets. A journal alone can never authorize recovery.
/// `running_pin` must come from the fixed service invocation, not the journal.
pub fn validate_transition(
    root: &Path,
    transition: &crate::activation_transaction::Transition,
    executable: &Path,
    executable_digest: &str,
    running_pin: &str,
) -> PackageResult<()> {
    let before: BootstrapConfig =
        serde_json::from_slice(&transition.bootstrap.before).map_err(|e| e.to_string())?;
    let old: ActivationState =
        serde_json::from_slice(&transition.selection.before).map_err(|e| e.to_string())?;
    let new: ActivationState =
        serde_json::from_slice(&transition.selection.after).map_err(|e| e.to_string())?;
    let old_digest = old
        .active
        .as_deref()
        .ok_or("old package identity missing")?;
    let new_digest = new
        .active
        .as_deref()
        .ok_or("new package identity missing")?;
    if let Some(change) = &transition.input_policy {
        let expected =
            crate::input_companion::retarget_policy(&change.before, old_digest, new_digest)?;
        if expected != change.after {
            return Err("input companion capabilities changed during activation".into());
        }
    } else if root.join("input-companion-policy.json").exists() {
        return Err("input companion authorization is missing from activation transaction".into());
    }
    if running_pin != old_digest && running_pin != new_digest {
        return Err("service pin is unrelated to transition".into());
    }
    let origin = new
        .repository_sets
        .get(new_digest)
        .ok_or("candidate provenance missing")?;
    let allowlist = read_allowlist(root)?;
    if origin.key_fingerprint != allowlist.publisher_fingerprint {
        return Err("candidate publisher differs from host authority".into());
    }
    let revision = sha256(&transition.selection.before);
    let context = PolicyCandidateContext {
        selection_revision: &revision,
        publisher_fingerprint: &origin.key_fingerprint,
        manifest_sha256: new_digest,
    };
    let (candidate_policy, _) =
        candidate_from_allowlist(&allowlist, &context, &transition.bootstrap.before)?;
    if candidate_policy != transition.bootstrap.after {
        return Err("trial policy differs from host authority".into());
    }
    let proposed: BootstrapConfig =
        serde_json::from_slice(&candidate_policy).map_err(|e| e.to_string())?;
    let old_package = amaterasu_package::verify_package(
        &amaterasu_package::package_directory(root, old_digest)?,
        &before,
        amaterasu_package::VerificationPolicy {
            minimum_sequence: 0,
            allow_historical: true,
            now: 0,
        },
    )?;
    if !old_package.signed
        || old_package.digest != old_digest
        || old_package.manifest.host_context_schema != 1
        || old_package.manifest.sequence > old.highest_sequence
    {
        return Err("previous package is not a signed known selection".into());
    }
    let new_package = amaterasu_package::verify_package(
        &amaterasu_package::package_directory(root, new_digest)?,
        &proposed,
        amaterasu_package::VerificationPolicy {
            minimum_sequence: old.highest_sequence,
            allow_historical: false,
            now: 0,
        },
    )?;
    if !new_package.signed
        || new_package.digest != new_digest
        || new_package.manifest.host_context_schema != 1
        || new_package.signer_fingerprint.as_deref() != Some(origin.key_fingerprint.as_str())
    {
        return Err("trial package is not signed by the authorized publisher".into());
    }
    let request = HostPackageActivation {
        repository_id: origin.repository_id.clone(),
        catalog_digest: "0".repeat(64),
        manifest_digest: new_digest.into(),
        expected_active: old_digest.into(),
        expected_revision: revision,
    };
    let expected = next_selection(
        &old,
        &request,
        &origin.key_fingerprint,
        new_package.manifest.sequence,
    )?;
    if serde_json::to_value(&expected).map_err(|e| e.to_string())?
        != serde_json::to_value(&new).map_err(|e| e.to_string())?
    {
        return Err("selection transition contains unauthorized changes".into());
    }
    let old_unit = amaterasu_package::linux_user_unit::render_service_unit(
        executable,
        root,
        old_digest,
        executable_digest,
    )?;
    let new_unit = amaterasu_package::linux_user_unit::render_service_unit(
        executable,
        root,
        new_digest,
        executable_digest,
    )?;
    if transition.unit.before != old_unit.as_bytes() || transition.unit.after != new_unit.as_bytes()
    {
        return Err("service unit transition does not match pinned host identity".into());
    }
    Ok(())
}

/// The user manager identifies the actual unit; browser input never supplies a
/// filesystem path. A different running process or an unexpected response is
/// not an authorized service unit.
fn managed_unit() -> PackageResult<std::path::PathBuf> {
    use std::{
        io::Read,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let mut child = Command::new("/usr/bin/systemctl")
        .args([
            "--user",
            "show",
            "amaterasu-linux-session.service",
            "--property=FragmentPath",
            "--property=MainPID",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("systemd unit query timed out".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if !status.success() {
        return Err("systemd unit query failed".into());
    }
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .ok_or("systemd unit output unavailable")?
        .take(65_537)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 65_536 {
        return Err("systemd unit response is too large".into());
    }
    let output = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    let mut fragment = None;
    let mut pid = None;
    for line in output.lines() {
        if let Some(value) = line.strip_prefix("FragmentPath=") {
            if fragment.replace(value).is_some() {
                return Err("duplicate systemd fragment".into());
            }
        } else if let Some(value) = line.strip_prefix("MainPID=") {
            if pid.replace(value).is_some() {
                return Err("duplicate systemd process".into());
            }
        } else {
            return Err("unexpected systemd unit response".into());
        }
    }
    if pid != Some(std::process::id().to_string().as_str()) {
        return Err("systemd unit does not own this host process".into());
    }
    let path = std::path::PathBuf::from(fragment.ok_or("systemd fragment missing")?);
    if !path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err("invalid systemd unit path".into());
    }
    let mut current = std::path::PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if fs::symlink_metadata(&current)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("systemd unit path contains a symlink".into());
        }
    }
    Ok(path)
}

fn reload_user_manager() -> PackageResult<()> {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let mut child = Command::new("/usr/bin/systemctl")
        .args(["--user", "daemon-reload"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err("systemd user reload failed".into())
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("systemd user reload timed out".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn host_executable() -> PackageResult<(std::path::PathBuf, String)> {
    let executable = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let hash =
        amaterasu_package::sha256_reader(fs::File::open(&executable).map_err(|e| e.to_string())?)?;
    Ok((executable, hash))
}

/// Recover an interrupted transaction before evaluating the service pin.
/// Reconstructs policy and signed set identity from host authority, never from
/// the journal alone. The caller holds the root lock.
pub fn recover(root: &Path, running_pin: &str) -> PackageResult<bool> {
    let journal = root.join("package-activation-journal.json");
    match fs::symlink_metadata(&journal) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    }
    let unit = managed_unit()?;
    let bootstrap = root.join("bootstrap.json");
    let selection = root.join("selection.json");
    let (executable, hash) = host_executable()?;
    let input_policy = root.join("input-companion-policy.json");
    let paths = crate::activation_transaction::Paths {
        input_policy: &input_policy,
        journal: &journal,
        bootstrap: &bootstrap,
        selection: &selection,
        unit: &unit,
    };
    crate::activation_transaction::recover(
        &paths,
        &mut |transition| validate_transition(root, transition, &executable, &hash, running_pin),
        &mut reload_user_manager,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupTransition {
    None,
    Trial,
    Recovered,
}

/// A completely written transition may be tried once under its new unit pin.
/// Partial writes or an old pin are rolled back before starting components.
pub fn startup_transition(root: &Path, running_pin: &str) -> PackageResult<StartupTransition> {
    let journal = root.join("package-activation-journal.json");
    match fs::symlink_metadata(&journal) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StartupTransition::None);
        }
        Err(error) => return Err(error.to_string()),
    }
    let unit = managed_unit()?;
    let bootstrap = root.join("bootstrap.json");
    let selection = root.join("selection.json");
    let (executable, hash) = host_executable()?;
    let input_policy = root.join("input-companion-policy.json");
    let paths = crate::activation_transaction::Paths {
        input_policy: &input_policy,
        journal: &journal,
        bootstrap: &bootstrap,
        selection: &selection,
        unit: &unit,
    };
    evaluate_startup(
        root,
        running_pin,
        &paths,
        &executable,
        &hash,
        &mut reload_user_manager,
    )
}

fn evaluate_startup(
    root: &Path,
    running_pin: &str,
    paths: &crate::activation_transaction::Paths<'_>,
    executable: &Path,
    hash: &str,
    reload: &mut impl FnMut() -> PackageResult<()>,
) -> PackageResult<StartupTransition> {
    let mut authorize = |transition: &crate::activation_transaction::Transition| {
        validate_transition(root, transition, executable, hash, running_pin)
    };
    if let Ok((transition, false)) =
        crate::activation_transaction::ready_for_trial(paths, &mut authorize)
    {
        let selected: ActivationState =
            serde_json::from_slice(&transition.selection.after).map_err(|e| e.to_string())?;
        if selected.active.as_deref() == Some(running_pin) {
            crate::activation_transaction::claim_trial(paths, &mut authorize)?;
            return Ok(StartupTransition::Trial);
        }
    }
    crate::activation_transaction::recover(paths, &mut authorize, reload)?;
    Ok(StartupTransition::Recovered)
}

/// Confirm the new selected package after its worker has started and passed
/// health; until this succeeds the journal remains available for rollback.
pub fn finalize(root: &Path, running_pin: &str) -> PackageResult<()> {
    let journal = root.join("package-activation-journal.json");
    let unit = managed_unit()?;
    let bootstrap = root.join("bootstrap.json");
    let selection = root.join("selection.json");
    let (executable, hash) = host_executable()?;
    let input_policy = root.join("input-companion-policy.json");
    let paths = crate::activation_transaction::Paths {
        input_policy: &input_policy,
        journal: &journal,
        bootstrap: &bootstrap,
        selection: &selection,
        unit: &unit,
    };
    crate::activation_transaction::finalize_trial(&paths, &mut |transition| {
        validate_transition(root, transition, &executable, &hash, running_pin)?;
        let selected: ActivationState =
            serde_json::from_slice(&transition.selection.after).map_err(|e| e.to_string())?;
        if selected.active.as_deref() != Some(running_pin) {
            return Err("trial service does not match selected package".into());
        }
        Ok(())
    })
}

/// Stage and health-check a candidate, then commit policy, selection and the
/// fixed unit pin as one recoverable host transaction. The caller has stopped
/// the old components, revoked input, and holds the host root lock.
pub fn activate(
    root: &Path,
    running_pin: &str,
    state_root: &Path,
    request: &HostPackageActivation,
) -> PackageResult<()> {
    if request.expected_active != running_pin {
        return Err("activation request does not match the running service".into());
    }
    let bootstrap_path = root.join("bootstrap.json");
    let selection_path = root.join("selection.json");
    let journal = root.join("package-activation-journal.json");
    let old_bootstrap = read_bounded(&bootstrap_path, 1024 * 1024)?;
    let old_selection = read_bounded(&selection_path, 1024 * 1024)?;
    let selection: ActivationState =
        serde_json::from_slice(&old_selection).map_err(|e| e.to_string())?;
    let unit = managed_unit()?;
    let old_unit = read_bounded(&unit, 1024 * 1024)?;
    let (executable, host_hash) = host_executable()?;
    let ((sequence, signer), proposed) =
        prepare_trial(root, request, |path, verified, bootstrap| {
            let mut package = amaterasu_package::verify_package(
                path,
                bootstrap,
                amaterasu_package::VerificationPolicy {
                    minimum_sequence: verified.manifest.sequence,
                    allow_historical: false,
                    now: unix_timestamp(),
                },
            )?;
            if package.digest != verified.digest
                || package.signer_fingerprint != verified.signer_fingerprint
            {
                return Err("candidate changed before its trial".into());
            }
            let logging = crate::logging_config::HostLoggingConfig::from_global(
                bootstrap.policy.settings.get("global"),
                state_root,
            )?;
            logging.inject(&mut package.runtime_config)?;
            let mut loaded = crate::loading::LoadedComponents::load(
                root,
                path,
                &package,
                state_root,
                logging.start_product_logger(),
                &mut |_| Ok(()),
            )?;
            loaded.tick()?;
            loaded.health()?;
            loaded.quiesce(std::time::Duration::from_secs(20))?;
            drop(loaded);
            Ok((
                package.manifest.sequence,
                package
                    .signer_fingerprint
                    .ok_or("candidate signer unavailable")?,
            ))
        })?;
    let next = next_selection(&selection, request, &signer, sequence)?;
    let new_unit = amaterasu_package::linux_user_unit::render_service_unit(
        &executable,
        root,
        &request.manifest_digest,
        &host_hash,
    )?;
    let transition = crate::activation_transaction::Transition {
        input_policy: prepare_input_transition(
            root,
            &request.expected_active,
            &request.manifest_digest,
        )?,
        bootstrap: crate::activation_transaction::Change {
            before: old_bootstrap,
            after: proposed,
        },
        selection: crate::activation_transaction::Change {
            before: old_selection,
            after: serde_json::to_vec_pretty(&next).map_err(|e| e.to_string())?,
        },
        unit: crate::activation_transaction::Change {
            before: old_unit,
            after: new_unit.into_bytes(),
        },
    };
    let input_policy = root.join("input-companion-policy.json");
    let paths = crate::activation_transaction::Paths {
        input_policy: &input_policy,
        journal: &journal,
        bootstrap: &bootstrap_path,
        selection: &selection_path,
        unit: &unit,
    };
    // Keep publisher approval and catalog identity locked through the complete
    // transaction, including the unit reload and possible rollback.
    let store = RepositoryStore::new(root.join("repositories"))?;
    store.with_current_catalog(
        &request.repository_id,
        &request.catalog_digest,
        &amaterasu_package::current_target(),
        unix_timestamp(),
        |approved, catalog| {
            if approved.fingerprint != signer
                || !catalog.candidates().iter().any(|c| {
                    c.kind == amaterasu_package::repository::CandidateKind::NativeSet
                        && c.manifest_sha256 == request.manifest_digest
                })
            {
                return Err("candidate publisher approval changed before activation".into());
            }
            // Recheck expiration and bytes at the commit boundary. Recovery
            // deliberately validates historical signatures without requiring
            // an unexpired candidate so a failed trial can still roll back.
            let commit_bootstrap: BootstrapConfig =
                serde_json::from_slice(&transition.bootstrap.after).map_err(|e| e.to_string())?;
            let current = amaterasu_package::verify_package(
                &amaterasu_package::package_directory(root, &request.manifest_digest)?,
                &commit_bootstrap,
                amaterasu_package::VerificationPolicy {
                    minimum_sequence: sequence,
                    allow_historical: false,
                    now: unix_timestamp(),
                },
            )?;
            if !current.signed
                || current.digest != request.manifest_digest
                || current.signer_fingerprint.as_deref() != Some(signer.as_str())
            {
                return Err("candidate changed or expired before activation".into());
            }
            let mut authorize = |observed: &crate::activation_transaction::Transition| {
                if observed != &transition {
                    return Err("activation journal differs from the approved transition".into());
                }
                validate_transition(root, observed, &executable, &host_hash, running_pin)
            };
            crate::activation_transaction::commit(
                &paths,
                &transition,
                &mut authorize,
                &mut reload_user_manager,
            )
        },
    )
}

/// Durable, privacy-limited progress for management after a worker restart.
/// This is a receipt only and cannot authorize a package or policy change.
pub fn write_status(state_root: &Path, digest: &str, status: &str) -> PackageResult<()> {
    if !amaterasu_package::linux_user_unit::valid_digest(digest)
        || !matches!(status, "activating" | "committed" | "failed")
    {
        return Err("invalid activation receipt".into());
    }
    let path = state_root.join("package-activation-status.json");
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            // SAFETY: geteuid has no arguments or pointer requirements.
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o777 != 0o600
            {
                return Err("activation receipt is not a private user-owned file".into());
            }
            let prior: serde_json::Value =
                serde_json::from_slice(&read_bounded(&path, 4096)?).map_err(|e| e.to_string())?;
            if prior.get("schema").and_then(|v| v.as_u64()) != Some(1) {
                return Err("unexpected existing activation receipt".into());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    amaterasu_package::write_json_atomically(
        &path,
        &serde_json::json!({"schema":1,"digest":digest,"status":status}),
    )
}

fn prepare_input_transition(
    root: &Path,
    old: &str,
    new: &str,
) -> PackageResult<Option<crate::activation_transaction::Change>> {
    let path = root.join("input-companion-policy.json");
    if !path.try_exists().map_err(|e| e.to_string())? {
        return Ok(None);
    }
    let before = read_bounded(&path, 1024 * 1024)?;
    let after = crate::input_companion::retarget_policy(&before, old, new)?;
    Ok(Some(crate::activation_transaction::Change {
        before,
        after,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_transition_and_crash_recovery_bind_policy_packages_and_unit() {
        use amaterasu_contracts::{Grants, RuntimeConfig};
        use amaterasu_package::{
            ComponentManifest, ReleaseManifest, ReleaseSignature, encode_hex, random_identifier,
            write_json_atomically,
        };
        use ed25519_dalek::{Signer, SigningKey};
        use std::collections::BTreeMap;
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "amaterasu-activation-signed-{}",
            random_identifier().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let signer = SigningKey::from_bytes(&[61; 32]);
        let fingerprint = sha256(signer.verifying_key().as_bytes());
        let make_set = |sequence: u64| {
            let bytes = format!("runtime-{sequence}").into_bytes();
            let manifest = ReleaseManifest {
                schema: 1,
                host_context_schema: 1,
                project: "amaterasu".into(),
                sequence,
                expires_unix: unix_timestamp() + 3600,
                target: amaterasu_package::current_target(),
                components: vec![ComponentManifest {
                    id: "amaterasu.runtime".into(),
                    version: "1".into(),
                    file: "runtime.bin".into(),
                    sha256: sha256(&bytes),
                    size: bytes.len() as u64,
                    abi_major: amaterasu_abi::ABI_MAJOR,
                    runtime: true,
                    requires: BTreeMap::new(),
                    requested: Grants::default(),
                }],
                privileged_requests: Vec::new(),
            };
            let raw = serde_json::to_vec(&manifest).unwrap();
            let digest = sha256(&raw);
            let directory = amaterasu_package::package_directory(&root, &digest).unwrap();
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("runtime.bin"), bytes).unwrap();
            fs::write(directory.join("manifest.json"), &raw).unwrap();
            write_json_atomically(
                &directory.join("manifest.sig"),
                &ReleaseSignature {
                    key_id: fingerprint.clone(),
                    signature: encode_hex(&signer.sign(&raw).to_bytes()),
                },
            )
            .unwrap();
            digest
        };
        let old_digest = make_set(10);
        let new_digest = make_set(11);
        let mut policy = RuntimeConfig::default();
        policy.principals.insert(
            "amaterasu.management".into(),
            Grants {
                call: [amaterasu_contracts::HOST_PACKAGE_ACTIVATION_SERVICE.to_owned()].into(),
                ..Default::default()
            },
        );
        policy.settings.insert(
            "amaterasu.management".into(),
            serde_json::json!({"enabled":false}),
        );
        let bootstrap = BootstrapConfig {
            development: false,
            allow_unsigned: false,
            trusted_keys: [(
                fingerprint.clone(),
                encode_hex(signer.verifying_key().as_bytes()),
            )]
            .into(),
            policy,
        };
        let before_bootstrap = serde_json::to_vec_pretty(&bootstrap).unwrap();
        let mut after_bootstrap = bootstrap.clone();
        after_bootstrap.policy.settings.insert(
            "amaterasu.management".into(),
            serde_json::json!({"enabled":true}),
        );
        let after_bootstrap = serde_json::to_vec_pretty(&after_bootstrap).unwrap();
        let old_selection = ActivationState {
            active: Some(old_digest.clone()),
            highest_sequence: 10,
            ..Default::default()
        };
        let old_selection_bytes = serde_json::to_vec_pretty(&old_selection).unwrap();
        let store = RepositoryStore::new(root.join("repositories")).unwrap();
        store
            .approve(
                amaterasu_package::repository::ApprovedRepository::approve(
                    "approved".into(),
                    "https://updates.example.test".into(),
                    encode_hex(signer.verifying_key().as_bytes()),
                    fingerprint.clone(),
                )
                .unwrap(),
            )
            .unwrap();
        let candidate = amaterasu_package::repository::Candidate {
            slot: "amaterasu.runtime".into(),
            implementation: "fixture".into(),
            version: "1".into(),
            kind: amaterasu_package::repository::CandidateKind::NativeSet,
            manifest_sha256: new_digest.clone(),
        };
        let catalog = amaterasu_package::repository::Catalog {
            schema: 1,
            repository: "approved".into(),
            sequence: 1,
            expires_unix: unix_timestamp() + 3600,
            target: amaterasu_package::current_target(),
            candidates: vec![candidate],
        };
        let catalog_raw = serde_json::to_vec(&catalog).unwrap();
        let mut signed = b"amaterasu.repository.catalog.v1\n".to_vec();
        signed.extend_from_slice(&catalog_raw);
        store
            .accept_catalog(
                "approved",
                &catalog_raw,
                &encode_hex(&signer.sign(&signed).to_bytes()),
                &amaterasu_package::current_target(),
                unix_timestamp(),
            )
            .unwrap();
        let cached = store
            .native_candidate_cache_path("approved", &new_digest)
            .unwrap();
        fs::create_dir(&cached).unwrap();
        let original_set = amaterasu_package::package_directory(&root, &new_digest).unwrap();
        for file in ["manifest.json", "manifest.sig", "runtime.bin"] {
            fs::copy(original_set.join(file), cached.join(file)).unwrap();
        }
        fs::remove_dir_all(original_set).unwrap();
        let request = HostPackageActivation {
            repository_id: "approved".into(),
            catalog_digest: sha256(&catalog_raw),
            manifest_digest: new_digest.clone(),
            expected_active: old_digest.clone(),
            expected_revision: sha256(&old_selection_bytes),
        };
        let context = PolicyCandidateContext {
            selection_revision: &request.expected_revision,
            publisher_fingerprint: &fingerprint,
            manifest_sha256: &new_digest,
        };
        let approval = amaterasu_package::policy_approval::propose_policy_approval(
            &before_bootstrap,
            &after_bootstrap,
            &context,
        )
        .unwrap();
        let allowlist = PolicyAllowlist {
            publisher_fingerprint: fingerprint.clone(),
            changes: approval.changes,
        };
        let (after_bootstrap, _) =
            candidate_from_allowlist(&allowlist, &context, &before_bootstrap).unwrap();
        let policy_path = root.join("package-activation-policy.json");
        write_json_atomically(&policy_path, &allowlist).unwrap();
        fs::write(root.join("bootstrap.json"), &before_bootstrap).unwrap();
        fs::write(root.join("selection.json"), &old_selection_bytes).unwrap();
        let host_lock = amaterasu_package::RootLock::acquire(&root).unwrap();
        let (sequence, staged_policy) = prepare_trial(&root, &request, |path, package, _policy| {
            assert_eq!(package.digest, new_digest);
            assert_ne!(path, cached);
            assert!(store.revoke("approved").is_err());
            Ok(package.manifest.sequence)
        })
        .unwrap();
        assert_eq!(sequence, 11);
        assert_eq!(staged_policy, after_bootstrap);
        assert_eq!(
            fs::read(root.join("selection.json")).unwrap(),
            old_selection_bytes
        );
        drop(host_lock);
        let executable = Path::new("/usr/bin/amaterasu-test");
        let host_hash = "a".repeat(64);
        let old_unit = amaterasu_package::linux_user_unit::render_service_unit(
            executable,
            &root,
            &old_digest,
            &host_hash,
        )
        .unwrap();
        let new_unit = amaterasu_package::linux_user_unit::render_service_unit(
            executable,
            &root,
            &new_digest,
            &host_hash,
        )
        .unwrap();
        let transition = crate::activation_transaction::Transition {
            input_policy: None,
            bootstrap: crate::activation_transaction::Change {
                before: before_bootstrap,
                after: after_bootstrap,
            },
            selection: crate::activation_transaction::Change {
                before: old_selection_bytes,
                after: serde_json::to_vec_pretty(
                    &next_selection(&old_selection, &request, &fingerprint, 11).unwrap(),
                )
                .unwrap(),
            },
            unit: crate::activation_transaction::Change {
                before: old_unit.into_bytes(),
                after: new_unit.into_bytes(),
            },
        };
        validate_transition(&root, &transition, executable, &host_hash, &old_digest).unwrap();
        let mut with_input = transition.clone();
        let input_before = serde_json::to_vec(&serde_json::json!({"schema":1,"component_set_digest":old_digest,"keyboard":true,"mouse":false,"gamepad":false})).unwrap();
        let input_after =
            crate::input_companion::retarget_policy(&input_before, &old_digest, &new_digest)
                .unwrap();
        with_input.input_policy = Some(crate::activation_transaction::Change {
            before: input_before,
            after: input_after,
        });
        validate_transition(&root, &with_input, executable, &host_hash, &old_digest).unwrap();
        let change = with_input.input_policy.as_mut().unwrap();
        let mut widened: serde_json::Value = serde_json::from_slice(&change.after).unwrap();
        widened["mouse"] = serde_json::json!(true);
        change.after = serde_json::to_vec_pretty(&widened).unwrap();
        assert!(
            validate_transition(&root, &with_input, executable, &host_hash, &old_digest).is_err()
        );
        let mut forged = transition.clone();
        forged.unit.after.extend_from_slice(b"changed");
        assert!(validate_transition(&root, &forged, executable, &host_hash, &old_digest).is_err());
        let mut forged = transition.clone();
        forged.bootstrap.after = transition.bootstrap.before.clone();
        assert!(validate_transition(&root, &forged, executable, &host_hash, &old_digest).is_err());
        let b = root.join("bootstrap.json");
        let s = root.join("selection.json");
        let u = root.join("unit");
        let j = root.join("journal");
        let input_policy = root.join("input-companion-policy.json");
        let paths = crate::activation_transaction::Paths {
            input_policy: &input_policy,
            journal: &j,
            bootstrap: &b,
            selection: &s,
            unit: &u,
        };
        fs::write(&b, &transition.bootstrap.before).unwrap();
        fs::write(&s, &transition.selection.before).unwrap();
        fs::write(&u, &transition.unit.before).unwrap();
        crate::activation_transaction::commit(
            &paths,
            &transition,
            &mut |t| validate_transition(&root, t, executable, &host_hash, &old_digest),
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(
            evaluate_startup(
                &root,
                &new_digest,
                &paths,
                executable,
                &host_hash,
                &mut || Ok(())
            )
            .unwrap(),
            StartupTransition::Trial
        );
        // A second start means the one allowed trial crashed before health.
        assert_eq!(
            evaluate_startup(
                &root,
                &new_digest,
                &paths,
                executable,
                &host_hash,
                &mut || Ok(())
            )
            .unwrap(),
            StartupTransition::Recovered
        );
        assert_eq!(fs::read(&b).unwrap(), transition.bootstrap.before);
        crate::activation_transaction::commit(
            &paths,
            &transition,
            &mut |t| validate_transition(&root, t, executable, &host_hash, &old_digest),
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(
            evaluate_startup(
                &root,
                &new_digest,
                &paths,
                executable,
                &host_hash,
                &mut || Ok(())
            )
            .unwrap(),
            StartupTransition::Trial
        );
        crate::activation_transaction::finalize_trial(&paths, &mut |t| {
            validate_transition(&root, t, executable, &host_hash, &new_digest)
        })
        .unwrap();
        assert_eq!(fs::read(&b).unwrap(), transition.bootstrap.after);
        assert_eq!(fs::read(&s).unwrap(), transition.selection.after);
        assert_eq!(fs::read(&u).unwrap(), transition.unit.after);
        fs::write(&b, &transition.bootstrap.after).unwrap();
        fs::write(&s, &transition.selection.before).unwrap();
        fs::write(&u, &transition.unit.before).unwrap();
        fs::write(
            &j,
            serde_json::to_vec(
                &serde_json::json!({"schema":1,"trial_started":true,"transition":transition}),
            )
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(&j, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            crate::activation_transaction::recover(
                &paths,
                &mut |t| validate_transition(&root, t, executable, &host_hash, &old_digest),
                &mut || Ok(())
            )
            .unwrap()
        );
        assert_eq!(fs::read(&b).unwrap(), transition.bootstrap.before);
        assert_eq!(fs::read(&s).unwrap(), transition.selection.before);
        assert_eq!(fs::read(&u).unwrap(), transition.unit.before);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn activation_receipt_is_private_and_cannot_overwrite_an_unrelated_file() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "amaterasu-activation-receipt-{}",
            amaterasu_package::random_identifier().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("package-activation-status.json");
        write_status(&root, &"a".repeat(64), "activating").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        write_status(&root, &"a".repeat(64), "failed").unwrap();
        fs::write(&path, b"unrelated").unwrap();
        assert!(write_status(&root, &"a".repeat(64), "committed").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"unrelated");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selection_advances_only_after_a_new_signed_candidate() {
        let request = HostPackageActivation {
            repository_id: "approved".into(),
            catalog_digest: "1".repeat(64),
            manifest_digest: "2".repeat(64),
            expected_active: "3".repeat(64),
            expected_revision: "4".repeat(64),
        };
        let current = ActivationState {
            active: Some(request.expected_active.clone()),
            highest_sequence: 10,
            ..Default::default()
        };
        assert!(next_selection(&current, &request, &"5".repeat(64), 10).is_err());
        let next = next_selection(&current, &request, &"5".repeat(64), 11).unwrap();
        assert_eq!(
            next.active.as_deref(),
            Some(request.manifest_digest.as_str())
        );
        assert_eq!(
            next.previous.as_deref(),
            Some(request.expected_active.as_str())
        );
        assert!(next.healthy_sets.contains(&request.manifest_digest));
        assert_eq!(next.repository_floors[&"5".repeat(64)].sequence, 11);
        let mut pending = current.clone();
        pending.pending = Some("other".into());
        assert!(next_selection(&pending, &request, &"5".repeat(64), 11).is_err());
    }
}
