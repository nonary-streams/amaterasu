//! Explicit service-owner activation when the old package cannot start after
//! authenticated recovery. Uses the ordinary signed trial and durable commit.
use super::*;

/// The invoking local administrator must own the installation and its protected
/// authority. No network route calls this entry point. The old package is never
/// loaded, and recovery evidence is not rewritten to make it readable by it.
pub fn activate_stopped(root: &Path, request: &HostPackageActivation) -> PackageResult<()> {
    let service = StoppedService::identify(root, &request.expected_active)?;
    // prepare_trial rechecks the locked selection, installed publisher authority,
    // signed catalog and complete package. The same commit validates the old and
    // new pins, policy, input authorization and host hash before every write.
    activate_with_unit(
        &service.root,
        &request.expected_active,
        &service.state_root,
        request,
        &service.unit,
    )
}

/// Return a stopped installation to a retained set it ran before, under the
/// same checks, trial and transaction as a restore the console asks for. This
/// is the way back from a set whose own console cannot switch sets.
pub fn restore_stopped(root: &Path, request: &HostPackageRestore) -> PackageResult<()> {
    let service = StoppedService::identify(root, &request.expected_active)?;
    restore::restore_with_unit(
        &service.root,
        &request.expected_active,
        &service.state_root,
        request,
        &service.unit,
    )
}

/// Compute the exact overlay revision before offline removal.
pub fn stopped_plugin_overlay_revision(
    root: &Path,
    expected_active: &str,
) -> PackageResult<String> {
    let service = StoppedService::identify(root, expected_active)?;
    amaterasu_package::plugins::installed::revision(&service.root, expected_active)
}

/// Recover the verified base after an installed overlay can no longer load.
pub fn remove_stopped_plugin_overlay(
    root: &Path,
    expected_active: &str,
    expected_overlay: &str,
) -> PackageResult<()> {
    let service = StoppedService::identify(root, expected_active)?;
    match fs::symlink_metadata(service.root.join("package-activation-journal.json")) {
        Ok(_) => return Err("offline plugin recovery requires completed base activation".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let plugin_operations = amaterasu_package::plugins::operation::OperationStore::new(
        service.state_root.join("plugin-installation"),
    )?;
    if plugin_operations
        .read()?
        .is_some_and(|operation| operation.in_flight.is_some())
    {
        return Err("offline plugin recovery requires completed plugin setup".into());
    }
    let selection: amaterasu_package::ActivationState =
        amaterasu_package::read_json(&service.root.join("selection.json"))?;
    if selection.active.as_deref() != Some(expected_active)
        || !selection.healthy_sets.contains(expected_active)
        || selection.pending.is_some()
        || selection.trial.is_some()
    {
        return Err("offline plugin recovery requires the exact healthy active base".into());
    }
    let bootstrap: amaterasu_package::BootstrapConfig =
        amaterasu_package::read_json(&service.root.join("bootstrap.json"))?;
    if bootstrap.development || bootstrap.allow_unsigned {
        return Err("offline plugin recovery requires an installed signed base".into());
    }
    crate::publisher_selection::with_verified_selected(
        &service.root,
        expected_active,
        &bootstrap,
        crate::publisher_selection::SelectionPurpose::RemoveStoppedPluginOverlay,
        &mut |_, _| Ok(()),
        |package| {
            if !package.signed
                || package.digest != expected_active
                || package.manifest.host_context_schema != 1
            {
                return Err("offline plugin recovery could not verify the pinned base".into());
            }
            amaterasu_package::plugins::installed::remove_stopped_overlay(
                &service.root,
                expected_active,
                expected_overlay,
            )
        },
    )
}

/// A stopped managed service whose unit names this host, this root and the
/// package the caller expects. The root lock is held while this value lives.
struct StoppedService {
    root: std::path::PathBuf,
    state_root: std::path::PathBuf,
    unit: std::path::PathBuf,
    _lock: amaterasu_package::RootLock,
}

impl StoppedService {
    fn identify(root: &Path, expected_active: &str) -> PackageResult<Self> {
        let root = crate::service::validate_user_service_root(root)?;
        let lock = amaterasu_package::RootLock::acquire(&root)?;
        let state_root = crate::service::validate_user_state_root(&root.join("data/state"))?;
        let unit = managed_unit_for(UnitOwner::Stopped)?;
        let metadata = fs::symlink_metadata(&unit).map_err(|e| e.to_string())?;
        // SAFETY: geteuid has no pointer or ownership requirements.
        let uid = unsafe { libc::geteuid() };
        if !metadata.is_file() || metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
            return Err("offline activation requires a protected user-owned service unit".into());
        }
        let (executable, hash) = host_executable()?;
        validate_unit_identity(
            &read_bounded(&unit, 1024 * 1024)?,
            &executable,
            &root,
            expected_active,
            &hash,
        )?;
        Ok(Self {
            root,
            state_root,
            unit,
            _lock: lock,
        })
    }
}

fn validate_unit_identity(
    observed: &[u8],
    executable: &Path,
    root: &Path,
    active: &str,
    executable_digest: &str,
) -> PackageResult<()> {
    let expected = amaterasu_package::linux_user_unit::render_service_unit(
        executable,
        root,
        active,
        executable_digest,
    )?;
    if observed != expected.as_bytes() {
        return Err("stopped service does not match this host, root and package pin".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_activation_requires_definitively_stopped_managed_service() {
        for state in ["inactive", "failed"] {
            let response = format!(
                "FragmentPath=/tmp/owned.service\nMainPID=0\nActiveState={state}\nDropInPaths=\nNeedDaemonReload=no\n"
            );
            assert!(parse_managed_unit(&response, UnitOwner::Stopped).is_ok());
        }
        for (pid, state, overrides, reload) in [
            ("0", "active", "", "no"),
            ("0", "activating", "", "no"),
            ("1", "inactive", "", "no"),
            ("0", "inactive", "/tmp/override.conf", "no"),
            ("0", "inactive", "", "yes"),
        ] {
            let response = format!(
                "FragmentPath=/tmp/owned.service\nMainPID={pid}\nActiveState={state}\nDropInPaths={overrides}\nNeedDaemonReload={reload}\n"
            );
            assert!(
                parse_managed_unit(&response, UnitOwner::Stopped).is_err(),
                "{response}"
            );
        }
        for response in [
            "FragmentPath=/tmp/owned.service\nMainPID=0\nActiveState=active\n",
            "FragmentPath=/tmp/owned.service\nMainPID=0\nActiveState=activating\n",
            "FragmentPath=/tmp/owned.service\nMainPID=1\nActiveState=inactive\n",
            "FragmentPath=/tmp/owned.service\nMainPID=0\n",
            "FragmentPath=/tmp/owned.service\nMainPID=0\nActiveState=inactive\nActiveState=failed\n",
            "FragmentPath=/tmp/owned.service\nMainPID=0\nMainPID=0\nActiveState=inactive\n",
            "FragmentPath=relative.service\nMainPID=0\nActiveState=inactive\n",
            "FragmentPath=/tmp/../owned.service\nMainPID=0\nActiveState=inactive\n",
        ] {
            assert!(
                parse_managed_unit(response, UnitOwner::Stopped).is_err(),
                "{response}"
            );
        }
        let response = format!(
            "FragmentPath=/tmp/owned.service\nMainPID={}\nActiveState=active\n",
            std::process::id()
        );
        assert!(parse_managed_unit(&response, UnitOwner::RunningHost).is_ok());
        assert!(parse_managed_unit(&response, UnitOwner::Stopped).is_err());
        assert!(
            parse_managed_unit(
                "FragmentPath=/tmp/owned.service\nMainPID=0\nActiveState=inactive\n",
                UnitOwner::RunningHost
            )
            .is_err()
        );
    }

    #[test]
    fn offline_activation_binds_exact_host_hash_root_and_current_pin() {
        let executable = Path::new("/tmp/owned-host");
        let root = Path::new("/tmp/owned-root");
        let pin = "a".repeat(64);
        let hash = "b".repeat(64);
        let unit =
            amaterasu_package::linux_user_unit::render_service_unit(executable, root, &pin, &hash)
                .unwrap();
        assert!(validate_unit_identity(unit.as_bytes(), executable, root, &pin, &hash).is_ok());
        for (host, directory, active, digest) in [
            (
                Path::new("/tmp/another-host"),
                root,
                pin.as_str(),
                hash.as_str(),
            ),
            (
                executable,
                Path::new("/tmp/another-root"),
                pin.as_str(),
                hash.as_str(),
            ),
            (executable, root, "c".repeat(64).as_str(), hash.as_str()),
            (executable, root, pin.as_str(), "d".repeat(64).as_str()),
        ] {
            assert!(
                validate_unit_identity(unit.as_bytes(), host, directory, active, digest).is_err()
            );
        }
        assert!(
            validate_unit_identity(
                format!("{unit}\nExecStart=/bin/sh\n").as_bytes(),
                executable,
                root,
                &pin,
                &hash
            )
            .is_err()
        );
    }
}
