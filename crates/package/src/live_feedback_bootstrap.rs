//! Host-owned, one-use evidence for a freshly provisioned installation root.
//!
//! Missing evidence never means an old installation was clean. This witness
//! is outside the State root and is consumed before modules can run.
use crate::{
    PackageResult, RootLock, create_private_directory, random_identifier, read_json,
    write_json_atomically,
};
use amaterasu_contracts::{
    LiveFeedbackBootstrapPhase, LiveFeedbackHostProof, LiveFeedbackProfile,
    LiveFeedbackStateOrigin, LiveFeedbackStateOriginPhase, RuntimeConfig,
};
use serde::{Deserialize, Serialize};
use std::path::Path;

const DIRECTORY: &str = ".live-feedback-origin";
const WITNESS: &str = "witness.json";
const STATE_SERVICE: &str = "amaterasu.state/1";
const STATE_ORIGIN_FILE: &str = ".live-feedback-state-origin-v1.json";

/// The State phase whose commit point is ambiguous across restart. The trusted
/// host quarantines rather than repeating potentially completed gate recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmedFeedbackBootstrapStatePhase {
    Active,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
enum Witness {
    Fresh {
        version: u8,
        installation_id: String,
    },
    Legacy {
        version: u8,
        installation_id: String,
    },
    Pending {
        version: u8,
        installation_id: String,
        profile: LiveFeedbackProfile,
        state_owner: String,
        state_root_identity: String,
        claim: String,
    },
    Initializing {
        version: u8,
        installation_id: String,
        profile: LiveFeedbackProfile,
        state_owner: String,
        state_root_identity: String,
        claim: String,
    },
    StateInitialized {
        version: u8,
        installation_id: String,
        profile: LiveFeedbackProfile,
        state_owner: String,
        state_root_identity: String,
        claim: String,
    },
    Consumed {
        version: u8,
        installation_id: String,
        profile: LiveFeedbackProfile,
        state_owner: String,
        state_root_identity: String,
    },
}

/// Explicit local setup creates an entirely new root. An existing root,
/// including an empty or partially removed old installation, is never adopted.
/// This provenance writer is currently available on Linux only.
#[cfg(target_os = "linux")]
pub fn create_new_root(root: &Path) -> PackageResult<()> {
    use std::os::unix::fs::DirBuilderExt;
    if !root.is_absolute() {
        return Err("feedback bootstrap root must be absolute".into());
    }
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(root)
        .map_err(|error| error.to_string())?;
    let origin = root.join(DIRECTORY);
    create_private_directory(&origin)?;
    write_json_atomically(
        &origin.join(WITNESS),
        &Witness::Fresh {
            version: 1,
            installation_id: random_identifier()?,
        },
    )?;
    amaterasu_storage::sync_directory(root)?;
    Ok(())
}

/// Called by the trusted loader for every startup. Pending claims survive a
/// crash, while retries after initialization begins must match persisted State.
#[cfg(target_os = "linux")]
pub fn prepare(
    root: &Path,
    state_root: &Path,
    config: &RuntimeConfig,
) -> PackageResult<Option<LiveFeedbackHostProof>> {
    let origin = root.join(DIRECTORY);
    let metadata = match std::fs::symlink_metadata(&origin) {
        Ok(value) => value,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound && config.live_feedback.is_none() =>
        {
            return Ok(None);
        }
        Err(_) => return Err("live feedback installation provenance is unavailable".into()),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("live feedback installation provenance is invalid".into());
    }
    let _lock = RootLock::acquire(&origin)?;
    let witness: Witness = read_json(&origin.join(WITNESS))?;
    let state_owner = || -> PackageResult<String> {
        let selected = config
            .service_selections
            .broker
            .get(STATE_SERVICE)
            .and_then(Option::as_ref)
            .ok_or("selected feedback State provider is missing")?;
        if selected.contract != STATE_SERVICE {
            return Err("selected feedback State contract is invalid".into());
        }
        Ok(selected.provider.clone())
    };
    let verified_state = |installation_id: &str, create_origin: bool| -> PackageResult<String> {
        let expected = canonical_state_root(root)?;
        if state_root != expected {
            return Err("feedback state root differs from the provisioned root".into());
        }
        let state_identity = state_root_identity(state_root)?;
        if create_origin {
            ensure_state_origin(state_root, installation_id, &state_identity)?;
        } else {
            verify_state_origin(state_root, installation_id, &state_identity)?;
        }
        Ok(state_identity)
    };
    match witness {
        Witness::Fresh {
            version: 1,
            installation_id,
        } if config.live_feedback.is_none() => {
            write_json_atomically(
                &origin.join(WITNESS),
                &Witness::Legacy {
                    version: 1,
                    installation_id,
                },
            )?;
            Ok(None)
        }
        Witness::Fresh {
            version: 1,
            installation_id,
        } => {
            let profile = config.live_feedback.clone().expect("selected profile");
            if config.effect_fence.is_some() || profile.host_configuration_owner.is_none() {
                return Err("fresh feedback requires selected host configuration and no historical generic authority".into());
            }
            let state_root_identity = verified_state(&installation_id, true)?;
            let state_owner = state_owner()?;
            let claim = random_identifier()?;
            write_json_atomically(
                &origin.join(WITNESS),
                &Witness::Pending {
                    version: 1,
                    installation_id: installation_id.clone(),
                    profile: profile.clone(),
                    state_owner: state_owner.clone(),
                    state_root_identity: state_root_identity.clone(),
                    claim: claim.clone(),
                },
            )?;
            Ok(Some(LiveFeedbackHostProof {
                version: 1,
                installation_id,
                profile,
                state_owner,
                state_root_identity,
                first_boot_claim: Some(claim),
                first_boot_phase: Some(LiveFeedbackBootstrapPhase::Pending),
            }))
        }
        Witness::Pending {
            version: 1,
            installation_id,
            profile,
            state_owner: previous_state,
            state_root_identity: previous_state_root_identity,
            claim,
        } => {
            let current_state_root_identity = verified_state(&installation_id, false)?;
            verify_pending_selection(
                config,
                &profile,
                &previous_state,
                &state_owner()?,
                &previous_state_root_identity,
                &current_state_root_identity,
            )?;
            Ok(Some(proof(
                installation_id,
                profile,
                previous_state,
                previous_state_root_identity,
                claim,
                LiveFeedbackBootstrapPhase::Pending,
            )))
        }
        Witness::Initializing {
            version: 1,
            installation_id,
            profile,
            state_owner: previous_state,
            state_root_identity: previous_state_root_identity,
            claim,
        } => {
            let current_state_root_identity = verified_state(&installation_id, false)?;
            verify_pending_selection(
                config,
                &profile,
                &previous_state,
                &state_owner()?,
                &previous_state_root_identity,
                &current_state_root_identity,
            )?;
            Ok(Some(proof(
                installation_id,
                profile,
                previous_state,
                previous_state_root_identity,
                claim,
                LiveFeedbackBootstrapPhase::Initializing,
            )))
        }
        Witness::StateInitialized {
            version: 1,
            installation_id,
            profile,
            state_owner: previous_state,
            state_root_identity: previous_state_root_identity,
            claim,
        } => {
            let current_state_root_identity = verified_state(&installation_id, false)?;
            verify_pending_selection(
                config,
                &profile,
                &previous_state,
                &state_owner()?,
                &previous_state_root_identity,
                &current_state_root_identity,
            )?;
            Ok(Some(proof(
                installation_id,
                profile,
                previous_state,
                previous_state_root_identity,
                claim,
                LiveFeedbackBootstrapPhase::StateInitialized,
            )))
        }
        Witness::Consumed {
            version: 1,
            installation_id,
            profile,
            state_owner: previous_state,
            state_root_identity: previous_state_root_identity,
        } => {
            let current_state_root_identity = verified_state(&installation_id, false)?;
            if config.live_feedback.as_ref() != Some(&profile)
                || config.effect_fence.is_some()
                || state_owner()? != previous_state
                || current_state_root_identity != previous_state_root_identity
            {
                return Err("previous feedback authority requires verified retirement before profile changes".into());
            }
            Ok(Some(LiveFeedbackHostProof {
                version: 1,
                installation_id,
                profile,
                state_owner: previous_state,
                state_root_identity: previous_state_root_identity,
                first_boot_claim: None,
                first_boot_phase: None,
            }))
        }
        Witness::Legacy { version: 1, .. } if config.live_feedback.is_none() => Ok(None),
        _ => Err("feedback bootstrap history is unavailable or requires cleanup".into()),
    }
}

fn proof(
    installation_id: String,
    profile: LiveFeedbackProfile,
    state_owner: String,
    state_root_identity: String,
    claim: String,
    phase: LiveFeedbackBootstrapPhase,
) -> LiveFeedbackHostProof {
    LiveFeedbackHostProof {
        version: 1,
        installation_id,
        profile,
        state_owner,
        state_root_identity,
        first_boot_claim: Some(claim),
        first_boot_phase: Some(phase),
    }
}

#[cfg(target_os = "linux")]
fn state_root_identity(path: &Path) -> PackageResult<String> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("feedback State root is not a real directory".into());
    }
    Ok(format!("{:016x}{:016x}", metadata.dev(), metadata.ino()))
}

#[cfg(target_os = "linux")]
fn valid_identifier(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(target_os = "linux")]
fn verify_proof_state_root(root: &Path, proof: &LiveFeedbackHostProof) -> PackageResult<()> {
    let expected = canonical_state_root(root)?;
    verify_state_origin(
        &expected,
        &proof.installation_id,
        &proof.state_root_identity,
    )
}

#[cfg(target_os = "linux")]
fn canonical_state_root(root: &Path) -> PackageResult<std::path::PathBuf> {
    let state_root = root.join("data/state");
    let metadata = std::fs::symlink_metadata(&state_root).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("feedback State root is not a real directory".into());
    }
    state_root.canonicalize().map_err(|error| error.to_string())
}

#[cfg(target_os = "linux")]
fn ensure_state_origin(
    state_root: &Path,
    installation_id: &str,
    state_root_identity: &str,
) -> PackageResult<()> {
    let path = state_root.join(STATE_ORIGIN_FILE);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            verify_state_origin(state_root, installation_id, state_root_identity)
        }
        Ok(_) => Err("feedback State origin marker is not a regular file".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_json_atomically(
                &path,
                &LiveFeedbackStateOrigin {
                    version: 1,
                    installation_id: installation_id.into(),
                    state_root_identity: state_root_identity.into(),
                    phase: LiveFeedbackStateOriginPhase::Fresh,
                    bootstrap_claim: None,
                },
            )?;
            amaterasu_storage::sync_directory(state_root)?;
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(target_os = "linux")]
fn verify_state_origin(
    state_root: &Path,
    installation_id: &str,
    state_root_identity: &str,
) -> PackageResult<()> {
    let path = state_root.join(STATE_ORIGIN_FILE);
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("feedback State origin marker is not a regular file".into());
    }
    let origin: LiveFeedbackStateOrigin = read_json(&path)?;
    let valid_claim = match (origin.phase, origin.bootstrap_claim.as_deref()) {
        (LiveFeedbackStateOriginPhase::Fresh | LiveFeedbackStateOriginPhase::Used, None) => true,
        (
            LiveFeedbackStateOriginPhase::BootstrapIntent
            | LiveFeedbackStateOriginPhase::BootstrapCommitted,
            Some(claim),
        ) => valid_identifier(claim),
        _ => false,
    };
    if origin.version != 1
        || origin.installation_id != installation_id
        || origin.state_root_identity != state_root_identity
        || !valid_claim
    {
        return Err("feedback State root was replaced after bootstrap preparation".into());
    }
    Ok(())
}

fn verify_pending_selection(
    config: &RuntimeConfig,
    expected_profile: &LiveFeedbackProfile,
    expected_state_owner: &str,
    selected_state_owner: &str,
    expected_state_root_identity: &str,
    selected_state_root_identity: &str,
) -> PackageResult<()> {
    if config.live_feedback.as_ref() != Some(expected_profile)
        || config.effect_fence.is_some()
        || expected_profile.host_configuration_owner.is_none()
        || selected_state_owner != expected_state_owner
        || selected_state_root_identity != expected_state_root_identity
    {
        return Err(
            "previous feedback authority requires verified retirement before profile changes"
                .into(),
        );
    }
    Ok(())
}

/// Persist the write-ahead phase before asking State to create the claim.
/// State's root marker distinguishes an unreceived request from lost history.
#[cfg(target_os = "linux")]
pub fn begin_state_initialization(root: &Path, proof: &LiveFeedbackHostProof) -> PackageResult<()> {
    verify_proof_state_root(root, proof)?;
    let (origin, _lock, witness) = read_locked(root)?;
    match &witness {
        Witness::Pending {
            version: 1,
            installation_id,
            profile,
            state_owner,
            state_root_identity,
            claim,
        } if matches_proof(
            installation_id,
            profile,
            state_owner,
            state_root_identity,
            claim,
            proof,
        ) =>
        {
            write_json_atomically(
                &origin.join(WITNESS),
                &Witness::Initializing {
                    version: 1,
                    installation_id: installation_id.clone(),
                    profile: profile.clone(),
                    state_owner: state_owner.clone(),
                    state_root_identity: state_root_identity.clone(),
                    claim: claim.clone(),
                },
            )?;
            Ok(())
        }
        Witness::Initializing {
            installation_id,
            profile,
            state_owner,
            state_root_identity,
            claim,
            ..
        }
        | Witness::StateInitialized {
            installation_id,
            profile,
            state_owner,
            state_root_identity,
            claim,
            ..
        } if matches_proof(
            installation_id,
            profile,
            state_owner,
            state_root_identity,
            claim,
            proof,
        ) =>
        {
            Ok(())
        }
        _ => Err("feedback bootstrap witness changed during State initialization".into()),
    }
}

/// Mark State initialization confirmed, retaining the claim until Pairing
/// commits its first owner epoch and opens the effect gate.
#[cfg(target_os = "linux")]
pub fn mark_state_initialized(root: &Path, proof: &LiveFeedbackHostProof) -> PackageResult<()> {
    verify_proof_state_root(root, proof)?;
    let (origin, _lock, witness) = read_locked(root)?;
    match &witness {
        Witness::Initializing {
            version: 1,
            installation_id,
            profile,
            state_owner,
            state_root_identity,
            claim,
        } if matches_proof(
            installation_id,
            profile,
            state_owner,
            state_root_identity,
            claim,
            proof,
        ) =>
        {
            write_json_atomically(
                &origin.join(WITNESS),
                &Witness::StateInitialized {
                    version: 1,
                    installation_id: installation_id.clone(),
                    profile: profile.clone(),
                    state_owner: state_owner.clone(),
                    state_root_identity: state_root_identity.clone(),
                    claim: claim.clone(),
                },
            )?;
            Ok(())
        }
        Witness::StateInitialized {
            installation_id,
            profile,
            state_owner,
            state_root_identity,
            claim,
            ..
        } if matches_proof(
            installation_id,
            profile,
            state_owner,
            state_root_identity,
            claim,
            proof,
        ) =>
        {
            Ok(())
        }
        _ => Err("feedback State claim was not initialized by this host".into()),
    }
}

/// Consume the claim only after Pairing has acknowledged gate recovery.
#[cfg(target_os = "linux")]
pub fn finish_after_pairing_recovery(
    root: &Path,
    proof: &LiveFeedbackHostProof,
) -> PackageResult<()> {
    verify_proof_state_root(root, proof)?;
    let (origin, _lock, witness) = read_locked(root)?;
    match &witness {
        Witness::StateInitialized {
            installation_id,
            profile,
            state_owner,
            state_root_identity,
            claim,
            ..
        } if matches_proof(
            installation_id,
            profile,
            state_owner,
            state_root_identity,
            claim,
            proof,
        ) =>
        {
            let consumed = consumed_from_state_initialized(&witness)?;
            write_json_atomically(&origin.join(WITNESS), &consumed)?;
            Ok(())
        }
        Witness::Consumed {
            version: 1,
            installation_id,
            profile,
            state_owner,
            state_root_identity,
        } if proof.first_boot_claim.as_deref().is_some()
            && proof.version == 1
            && proof.installation_id.as_str() == installation_id
            && &proof.profile == profile
            && proof.state_owner.as_str() == state_owner
            && proof.state_root_identity.as_str() == state_root_identity =>
        {
            Ok(())
        }
        _ => Err("feedback bootstrap witness cannot be finalized".into()),
    }
}

/// Quarantine a claim after the selected State provider confirms that a
/// previous Pairing process already started or completed recovery. The new
/// effect gate remains closed and the owner must use normal restart recovery.
#[cfg(target_os = "linux")]
pub fn quarantine_after_state_recovery(
    root: &Path,
    proof: &LiveFeedbackHostProof,
    state_phase: ConfirmedFeedbackBootstrapStatePhase,
) -> PackageResult<()> {
    if !matches!(
        proof.first_boot_phase,
        Some(
            LiveFeedbackBootstrapPhase::Initializing | LiveFeedbackBootstrapPhase::StateInitialized
        )
    ) {
        return Err("feedback bootstrap claim has no interrupted State initialization".into());
    }
    verify_proof_state_root(root, proof)?;
    let (origin, _lock, witness) = read_locked(root)?;
    match &witness {
        Witness::Initializing {
            installation_id,
            profile,
            state_owner,
            state_root_identity,
            claim,
            ..
        }
        | Witness::StateInitialized {
            installation_id,
            profile,
            state_owner,
            state_root_identity,
            claim,
            ..
        } if matches_proof(
            installation_id,
            profile,
            state_owner,
            state_root_identity,
            claim,
            proof,
        ) =>
        {
            // This typed value is supplied only after validating the selected
            // State provider's exact persisted response.
            let _confirmed_phase = state_phase;
            let consumed = consumed_from_interrupted(&witness)?;
            write_json_atomically(&origin.join(WITNESS), &consumed)?;
            Ok(())
        }
        _ => Err("feedback bootstrap witness cannot be quarantined".into()),
    }
}

#[cfg(target_os = "linux")]
fn read_locked(root: &Path) -> PackageResult<(std::path::PathBuf, RootLock, Witness)> {
    let origin = root.join(DIRECTORY);
    let metadata = std::fs::symlink_metadata(&origin).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("live feedback installation provenance is invalid".into());
    }
    let lock = RootLock::acquire(&origin)?;
    let witness = read_json(&origin.join(WITNESS))?;
    Ok((origin, lock, witness))
}

fn matches_proof(
    installation_id: &str,
    profile: &LiveFeedbackProfile,
    state_owner: &str,
    state_root_identity: &str,
    claim: &str,
    proof: &LiveFeedbackHostProof,
) -> bool {
    proof.version == 1
        && proof.installation_id == installation_id
        && proof.profile == *profile
        && proof.state_owner == state_owner
        && proof.state_root_identity == state_root_identity
        && proof.first_boot_claim.as_deref() == Some(claim)
}

fn consumed_from_state_initialized(witness: &Witness) -> PackageResult<Witness> {
    match witness {
        Witness::StateInitialized {
            version: 1,
            installation_id,
            profile,
            state_owner,
            state_root_identity,
            ..
        } => Ok(Witness::Consumed {
            version: 1,
            installation_id: installation_id.clone(),
            profile: profile.clone(),
            state_owner: state_owner.clone(),
            state_root_identity: state_root_identity.clone(),
        }),
        _ => Err("feedback bootstrap witness cannot be finalized".into()),
    }
}

#[cfg(target_os = "linux")]
fn consumed_from_interrupted(witness: &Witness) -> PackageResult<Witness> {
    match witness {
        Witness::Initializing {
            version: 1,
            installation_id,
            profile,
            state_owner,
            state_root_identity,
            ..
        }
        | Witness::StateInitialized {
            version: 1,
            installation_id,
            profile,
            state_owner,
            state_root_identity,
            ..
        } => Ok(Witness::Consumed {
            version: 1,
            installation_id: installation_id.clone(),
            profile: profile.clone(),
            state_owner: state_owner.clone(),
            state_root_identity: state_root_identity.clone(),
        }),
        _ => Err("feedback bootstrap witness cannot be quarantined".into()),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn prepare(
    _root: &Path,
    _state_root: &Path,
    config: &RuntimeConfig,
) -> PackageResult<Option<LiveFeedbackHostProof>> {
    if config.live_feedback.is_some() {
        return Err("live feedback first-boot provenance is not supported on this platform".into());
    }
    Ok(None)
}

#[cfg(not(target_os = "linux"))]
pub fn begin_state_initialization(
    _root: &Path,
    _proof: &LiveFeedbackHostProof,
) -> PackageResult<()> {
    Err("live feedback first-boot provenance is not supported on this platform".into())
}

#[cfg(not(target_os = "linux"))]
pub fn mark_state_initialized(_root: &Path, _proof: &LiveFeedbackHostProof) -> PackageResult<()> {
    Err("live feedback first-boot provenance is not supported on this platform".into())
}

#[cfg(not(target_os = "linux"))]
pub fn finish_after_pairing_recovery(
    _root: &Path,
    _proof: &LiveFeedbackHostProof,
) -> PackageResult<()> {
    Err("live feedback first-boot provenance is not supported on this platform".into())
}

#[cfg(not(target_os = "linux"))]
pub fn quarantine_after_state_recovery(
    _root: &Path,
    _proof: &LiveFeedbackHostProof,
    _state_phase: ConfirmedFeedbackBootstrapStatePhase,
) -> PackageResult<()> {
    Err("live feedback first-boot provenance is not supported on this platform".into())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use amaterasu_contracts::{SelectedService, ServiceSelections};
    use std::path::PathBuf;

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "amaterasu-feedback-bootstrap-{}",
                random_identifier().unwrap()
            )))
        }

        fn state_root(&self) -> PathBuf {
            let state_root = self.0.join("data/state");
            create_private_directory(&state_root).unwrap();
            state_root.canonicalize().unwrap()
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn selected_profile() -> RuntimeConfig {
        let profile = LiveFeedbackProfile {
            protocol_owner: "protocol".into(),
            pairing_owner: "pairing".into(),
            stream_owner: "stream".into(),
            input_owner: "input".into(),
            lifecycle_owner: "lifecycle".into(),
            control_owner: "control".into(),
            host_configuration_owner: Some("configuration".into()),
        };
        RuntimeConfig {
            live_feedback: Some(profile),
            service_selections: ServiceSelections {
                broker: [
                    (
                        STATE_SERVICE.into(),
                        Some(SelectedService {
                            provider: "state".into(),
                            contract: STATE_SERVICE.into(),
                        }),
                    ),
                    (
                        "amaterasu.host-configuration/1".into(),
                        Some(SelectedService {
                            provider: "configuration".into(),
                            contract: "amaterasu.host-configuration/1".into(),
                        }),
                    ),
                ]
                .into(),
                ..ServiceSelections::default()
            },
            ..RuntimeConfig::default()
        }
    }

    #[test]
    fn pending_claim_survives_interruption_before_state_initialization() {
        let root = TestRoot::new();
        create_new_root(&root.0).unwrap();
        let state_root = root.state_root();
        let config = selected_profile();

        let first = prepare(&root.0, &state_root, &config).unwrap().unwrap();
        let claim = first.first_boot_claim.as_ref().unwrap();
        assert_eq!(claim.len(), 32);
        assert_eq!(
            first.first_boot_phase,
            Some(LiveFeedbackBootstrapPhase::Pending)
        );
        assert_eq!(first.state_owner, "state");
        assert_eq!(first.profile, config.live_feedback.clone().unwrap());

        let restarted = prepare(&root.0, &state_root, &config).unwrap().unwrap();
        assert_eq!(restarted.installation_id, first.installation_id);
        assert_eq!(restarted.profile, first.profile);
        assert_eq!(restarted.first_boot_claim, first.first_boot_claim);
        assert_eq!(
            restarted.first_boot_phase,
            Some(LiveFeedbackBootstrapPhase::Pending)
        );
    }

    #[test]
    fn writeahead_witness_retries_after_crash_before_state_receives_the_claim() {
        let root = TestRoot::new();
        create_new_root(&root.0).unwrap();
        let state_root = root.state_root();
        let config = selected_profile();
        let first = prepare(&root.0, &state_root, &config).unwrap().unwrap();

        // This is the durable host boundary immediately before sending the
        // request. No State response exists yet.
        begin_state_initialization(&root.0, &first).unwrap();
        let restarted = prepare(&root.0, &state_root, &config).unwrap().unwrap();
        assert_eq!(restarted.first_boot_claim, first.first_boot_claim);
        assert_eq!(
            restarted.first_boot_phase,
            Some(LiveFeedbackBootstrapPhase::Initializing)
        );
        assert_eq!(restarted.state_root_identity, first.state_root_identity);
    }

    #[test]
    fn state_initialized_claim_resumes_until_pairing_finishes_recovery() {
        let root = TestRoot::new();
        create_new_root(&root.0).unwrap();
        let state_root = root.state_root();
        let config = selected_profile();
        let first = prepare(&root.0, &state_root, &config).unwrap().unwrap();

        begin_state_initialization(&root.0, &first).unwrap();
        let interrupted = prepare(&root.0, &state_root, &config).unwrap().unwrap();
        assert_eq!(interrupted.first_boot_claim, first.first_boot_claim);
        assert_eq!(
            interrupted.first_boot_phase,
            Some(LiveFeedbackBootstrapPhase::Initializing)
        );

        mark_state_initialized(&root.0, &interrupted).unwrap();
        let before_pairing_recovery = prepare(&root.0, &state_root, &config).unwrap().unwrap();
        assert_eq!(
            before_pairing_recovery.first_boot_claim,
            first.first_boot_claim
        );
        assert_eq!(
            before_pairing_recovery.first_boot_phase,
            Some(LiveFeedbackBootstrapPhase::StateInitialized)
        );

        finish_after_pairing_recovery(&root.0, &before_pairing_recovery).unwrap();
        let after_pairing_recovery = prepare(&root.0, &state_root, &config).unwrap().unwrap();
        assert_eq!(after_pairing_recovery.first_boot_claim, None);
        assert_eq!(after_pairing_recovery.first_boot_phase, None);
    }

    #[test]
    fn lost_state_after_initialization_attempt_is_not_adopted_as_fresh() {
        let root = TestRoot::new();
        create_new_root(&root.0).unwrap();
        let state_root = root.state_root();
        let config = selected_profile();
        let first = prepare(&root.0, &state_root, &config).unwrap().unwrap();
        begin_state_initialization(&root.0, &first).unwrap();
        mark_state_initialized(&root.0, &first).unwrap();

        create_private_directory(&state_root.join("namespaces")).unwrap();
        std::fs::remove_dir_all(state_root.join("namespaces")).unwrap();
        let resumed = prepare(&root.0, &state_root, &config).unwrap().unwrap();
        assert_eq!(resumed.first_boot_claim, first.first_boot_claim);
        assert_eq!(
            resumed.first_boot_phase,
            Some(LiveFeedbackBootstrapPhase::StateInitialized)
        );
    }

    #[test]
    fn pending_claim_rejects_a_replaced_state_root() {
        let root = TestRoot::new();
        create_new_root(&root.0).unwrap();
        let state_root = root.state_root();
        let config = selected_profile();
        let proof = prepare(&root.0, &state_root, &config).unwrap().unwrap();
        let displaced_root = root.0.join("data/state-displaced");
        std::fs::rename(&state_root, &displaced_root).unwrap();
        create_private_directory(&state_root).unwrap();

        assert!(prepare(&root.0, &state_root, &config).is_err());
        assert_ne!(
            state_root_identity(&state_root).unwrap(),
            proof.state_root_identity
        );
    }

    #[test]
    fn pending_claim_refuses_state_root_contents_loss_before_retry() {
        let root = TestRoot::new();
        create_new_root(&root.0).unwrap();
        let state_root = root.state_root();
        let config = selected_profile();
        let _proof = prepare(&root.0, &state_root, &config).unwrap().unwrap();
        let origin_path = state_root.join(STATE_ORIGIN_FILE);
        assert!(origin_path.exists());
        std::fs::remove_file(&origin_path).unwrap();

        assert!(prepare(&root.0, &state_root, &config).is_err());
        assert!(!origin_path.exists());
    }

    #[test]
    fn interrupted_pairing_recovery_is_quarantined_only_after_state_confirmation() {
        let root = TestRoot::new();
        create_new_root(&root.0).unwrap();
        let state_root = root.state_root();
        let config = selected_profile();
        let pending = prepare(&root.0, &state_root, &config).unwrap().unwrap();
        begin_state_initialization(&root.0, &pending).unwrap();
        mark_state_initialized(&root.0, &pending).unwrap();
        let restarted = prepare(&root.0, &state_root, &config).unwrap().unwrap();

        assert_eq!(
            quarantine_after_state_recovery(
                &root.0,
                &pending,
                ConfirmedFeedbackBootstrapStatePhase::Active,
            )
            .unwrap_err(),
            "feedback bootstrap claim has no interrupted State initialization"
        );
        assert_eq!(
            prepare(&root.0, &state_root, &config)
                .unwrap()
                .unwrap()
                .first_boot_claim,
            pending.first_boot_claim
        );

        quarantine_after_state_recovery(
            &root.0,
            &restarted,
            ConfirmedFeedbackBootstrapStatePhase::Active,
        )
        .unwrap();
        let quarantined = prepare(&root.0, &state_root, &config).unwrap().unwrap();
        assert_eq!(quarantined.first_boot_claim, None);
        assert_eq!(quarantined.first_boot_phase, None);
    }

    #[test]
    fn consumed_history_blocks_profile_removal_and_state_replacement() {
        let root = TestRoot::new();
        create_new_root(&root.0).unwrap();
        let state_root = root.state_root();
        let config = selected_profile();
        let proof = prepare(&root.0, &state_root, &config).unwrap().unwrap();
        begin_state_initialization(&root.0, &proof).unwrap();
        mark_state_initialized(&root.0, &proof).unwrap();
        finish_after_pairing_recovery(&root.0, &proof).unwrap();

        let mut removed = config.clone();
        removed.live_feedback = None;
        assert!(prepare(&root.0, &state_root, &removed).is_err());

        let mut replacement = config.clone();
        replacement
            .service_selections
            .broker
            .get_mut(STATE_SERVICE)
            .unwrap()
            .as_mut()
            .unwrap()
            .provider = "replacement-state".into();
        assert!(prepare(&root.0, &state_root, &replacement).is_err());
    }

    #[test]
    fn existing_or_interrupted_roots_are_never_adopted_as_fresh() {
        let existing = TestRoot::new();
        create_private_directory(&existing.0).unwrap();
        assert!(create_new_root(&existing.0).is_err());
        let state_root = existing.state_root();
        assert!(prepare(&existing.0, &state_root, &selected_profile()).is_err());

        let interrupted = TestRoot::new();
        create_private_directory(&interrupted.0).unwrap();
        create_private_directory(&interrupted.0.join(DIRECTORY)).unwrap();
        let state_root = interrupted.state_root();
        assert!(prepare(&interrupted.0, &state_root, &selected_profile()).is_err());
    }

    #[test]
    fn no_profile_at_first_start_consumes_fresh_evidence_as_legacy() {
        let root = TestRoot::new();
        create_new_root(&root.0).unwrap();
        let state_root = root.state_root();

        assert!(
            prepare(&root.0, &state_root, &RuntimeConfig::default())
                .unwrap()
                .is_none()
        );
        assert!(prepare(&root.0, &state_root, &selected_profile()).is_err());
    }
}
