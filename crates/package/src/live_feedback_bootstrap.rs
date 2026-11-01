//! Host-owned, one-use evidence for a freshly provisioned installation root.
//!
//! Missing evidence never means an old installation was clean. This witness
//! is outside the State root and is consumed before modules can run.
use crate::{
    PackageResult, RootLock, create_private_directory, random_identifier, read_json,
    write_json_atomically,
};
use amaterasu_contracts::{LiveFeedbackHostProof, LiveFeedbackProfile, RuntimeConfig};
use serde::{Deserialize, Serialize};
use std::path::Path;

const DIRECTORY: &str = ".live-feedback-origin";
const WITNESS: &str = "witness.json";
const STATE_SERVICE: &str = "amaterasu.state/1";

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
    Consumed {
        version: u8,
        installation_id: String,
        profile: LiveFeedbackProfile,
        state_owner: String,
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

/// Called by the trusted loader for every startup, including when the current
/// profile was removed. A consumed installation cannot silently change owners.
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
    let verified_state = || -> PackageResult<()> {
        let expected = root
            .join("data/state")
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if state_root != expected {
            return Err("feedback state root differs from the provisioned root".into());
        }
        Ok(())
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
            verified_state()?;
            let state_owner = state_owner()?;
            let claim = random_identifier()?;
            write_json_atomically(
                &origin.join(WITNESS),
                &Witness::Consumed {
                    version: 1,
                    installation_id: installation_id.clone(),
                    profile: profile.clone(),
                    state_owner: state_owner.clone(),
                },
            )?;
            Ok(Some(LiveFeedbackHostProof {
                version: 1,
                installation_id,
                profile,
                state_owner,
                first_boot_claim: Some(claim),
            }))
        }
        Witness::Consumed {
            version: 1,
            installation_id,
            profile,
            state_owner: previous_state,
        } => {
            verified_state()?;
            if config.live_feedback.as_ref() != Some(&profile)
                || config.effect_fence.is_some()
                || state_owner()? != previous_state
            {
                return Err("previous feedback authority requires verified retirement before profile changes".into());
            }
            Ok(Some(LiveFeedbackHostProof {
                version: 1,
                installation_id,
                profile,
                state_owner: previous_state,
                first_boot_claim: None,
            }))
        }
        Witness::Legacy { version: 1, .. } if config.live_feedback.is_none() => Ok(None),
        _ => Err("feedback bootstrap history is unavailable or requires cleanup".into()),
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
    fn fresh_setup_issues_one_claim_and_restart_never_reissues_it() {
        let root = TestRoot::new();
        create_new_root(&root.0).unwrap();
        let state_root = root.state_root();
        let config = selected_profile();

        let first = prepare(&root.0, &state_root, &config).unwrap().unwrap();
        let claim = first.first_boot_claim.as_ref().unwrap();
        assert_eq!(claim.len(), 32);
        assert_eq!(first.state_owner, "state");
        assert_eq!(first.profile, config.live_feedback.clone().unwrap());

        let restarted = prepare(&root.0, &state_root, &config).unwrap().unwrap();
        assert_eq!(restarted.installation_id, first.installation_id);
        assert_eq!(restarted.profile, first.profile);
        assert_eq!(restarted.first_boot_claim, None);
    }

    #[test]
    fn consumed_history_blocks_profile_removal_and_state_replacement() {
        let root = TestRoot::new();
        create_new_root(&root.0).unwrap();
        let state_root = root.state_root();
        let config = selected_profile();
        prepare(&root.0, &state_root, &config).unwrap();
        prepare(&root.0, &state_root, &config).unwrap();

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
