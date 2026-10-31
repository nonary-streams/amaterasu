//! Recovery of a retained native set is owned by the fixed host shell, not a
//! replaceable management or feature library. No candidate code is loaded here.
use amaterasu_package::{
    ActivationState, BootstrapConfig, PackageResult, RootLock, VerifiedPackage, read_bounded,
    read_json, write_json_atomically,
};
use std::path::Path;

pub fn restore(root: &Path, digest: Option<&str>) -> PackageResult<()> {
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    if !bootstrap.development {
        return Err("production native recovery requires installed-service authorization".into());
    }
    let path = root.join("selection.json");
    let selection: ActivationState = read_json(&path)?;
    let previous = match digest {
        Some(digest) => digest.to_owned(),
        None => selection.previous.clone().ok_or("no previous native set")?,
    };
    crate::publisher_selection::with_verified_selected(
        root,
        &previous,
        &bootstrap,
        crate::publisher_selection::SelectionPurpose::ExplicitRecovery,
        &mut |_, _| Ok(()),
        |verified| restore_verified_set(root, &bootstrap, &previous, digest.is_none(), verified),
    )?;
    println!(
        "Restored signed native set {previous}; accepted release floor remains intact. Its native code is authorized to execute under the user account at the next worker start."
    );
    Ok(())
}

fn restore_verified_set(
    root: &Path,
    expected_bootstrap: &BootstrapConfig,
    previous: &str,
    require_previous_pointer: bool,
    verified: VerifiedPackage,
) -> PackageResult<()> {
    let _lock = RootLock::acquire(root)?;
    require_current_development_bootstrap(root, expected_bootstrap)?;
    let path = root.join("selection.json");
    let mut selection: ActivationState = read_json(&path)?;
    if require_previous_pointer && selection.previous.as_deref() != Some(previous) {
        return Err("previous native set changed before recovery".into());
    }
    if selection.active.as_deref() == Some(previous) {
        return Err("requested native set is already active".into());
    }
    if selection.previous.as_deref() != Some(previous) && !selection.healthy_sets.contains(previous)
    {
        return Err("native recovery requires a previously healthy selected set".into());
    }
    if !verified.signed || verified.digest != previous {
        return Err("previous native set is not authenticated".into());
    }
    let old_active = selection.active.replace(previous.into());
    selection.previous = old_active;
    selection.pending = None;
    selection.trial = None;
    write_json_atomically(&path, &selection)
}

fn require_current_development_bootstrap(
    root: &Path,
    expected: &BootstrapConfig,
) -> PackageResult<()> {
    if !expected.development {
        return Err("production native recovery requires installed-service authorization".into());
    }
    let bytes = read_bounded(&root.join("bootstrap.json"), 1024 * 1024)?;
    let current: BootstrapConfig =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if !current.development {
        return Err("current host bootstrap is not in development mode".into());
    }
    let expected_bytes = serde_json::to_vec(expected).map_err(|error| error.to_string())?;
    let current_bytes = serde_json::to_vec(&current).map_err(|error| error.to_string())?;
    if current_bytes != expected_bytes {
        return Err("host bootstrap changed; reload before native recovery".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_contracts::RuntimeConfig;
    use amaterasu_package::{
        ReleaseManifest, current_target, random_identifier, write_json_atomically,
    };
    use std::collections::BTreeMap;
    use std::fs;

    #[test]
    fn a_policy_transition_during_verification_prevents_native_restore() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-native-recovery-transition-{}",
            random_identifier().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let expected_bootstrap = BootstrapConfig {
            development: true,
            allow_unsigned: false,
            trusted_keys: BTreeMap::new(),
            policy: RuntimeConfig::default(),
        };
        write_json_atomically(&root.join("bootstrap.json"), &expected_bootstrap).unwrap();
        let previous = "b".repeat(64);
        let before = ActivationState {
            active: Some("a".repeat(64)),
            previous: Some(previous.clone()),
            pending: Some("c".repeat(64)),
            trial: Some("c".repeat(64)),
            healthy_sets: [previous.clone()].into(),
            ..ActivationState::default()
        };
        write_json_atomically(&root.join("selection.json"), &before).unwrap();

        // Simulates a cooperating production transition completing after the
        // caller captured bootstrap and verified the set, before restore's
        // RootLock-protected mutation closure begins.
        let mut production_bootstrap = expected_bootstrap.clone();
        production_bootstrap.development = false;
        write_json_atomically(&root.join("bootstrap.json"), &production_bootstrap).unwrap();
        let verified = VerifiedPackage {
            service_diagnostics: Vec::new(),
            signer_fingerprint: None,
            privileged_requests: Vec::new(),
            manifest: ReleaseManifest {
                privileged_requests: Vec::new(),
                schema: 1,
                host_context_schema: 1,
                project: "amaterasu".into(),
                sequence: 1,
                expires_unix: u64::MAX,
                target: current_target(),
                components: Vec::new(),
            },
            digest: previous.clone(),
            signed: true,
            runtime_config: RuntimeConfig::default(),
        };

        assert!(
            restore_verified_set(&root, &expected_bootstrap, &previous, true, verified,).is_err()
        );
        let after: ActivationState = read_json(&root.join("selection.json")).unwrap();
        assert_eq!(after.active, before.active);
        assert_eq!(after.previous, before.previous);
        assert_eq!(after.pending, before.pending);
        assert_eq!(after.trial, before.trial);

        fs::remove_dir_all(root).unwrap();
    }
}
