//! Testable policy for best-effort installation of Steam's virtual render driver.

use std::path::{Path, PathBuf};
use std::time::Duration;

const ENDPOINT_ACTIVATION_POLL_INTERVAL: Duration = Duration::from_millis(500);
const ENDPOINT_ACTIVATION_MAX_POLLS: usize = 60;

pub(crate) fn steam_driver_inf_path(common_program_files_x86: &Path) -> PathBuf {
    common_program_files_x86
        .join("Steam")
        .join("drivers")
        .join("Windows10")
        .join("x64")
        .join("SteamStreamingSpeakers.inf")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DefaultRole {
    Console,
    Multimedia,
    Communications,
}

impl DefaultRole {
    pub(crate) const ALL: [Self; 3] = [Self::Console, Self::Multimedia, Self::Communications];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Console => "console",
            Self::Multimedia => "multimedia",
            Self::Communications => "communications",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Console => 0,
            Self::Multimedia => 1,
            Self::Communications => 2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DriverFailure {
    pub(crate) code: &'static str,
    pub(crate) native_code: Option<u32>,
}

impl DriverFailure {
    pub(crate) fn new(code: &'static str) -> Self {
        Self {
            code,
            native_code: None,
        }
    }

    pub(crate) fn native(code: &'static str, native_code: u32) -> Self {
        Self {
            code,
            native_code: Some(native_code),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RoleRestoreFailure {
    pub(crate) role: DefaultRole,
    pub(crate) failure: DriverFailure,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InstallOutcome {
    AlreadyPresent {
        endpoint: String,
    },
    Installed {
        endpoint: String,
        restored_roles: Vec<DefaultRole>,
        restore_failures: Vec<RoleRestoreFailure>,
    },
    InstalledEndpointUnavailable {
        restored_roles: Vec<DefaultRole>,
        restore_failures: Vec<RoleRestoreFailure>,
    },
}

pub(crate) trait SteamDriverBackend {
    fn steam_speaker_endpoint(&mut self) -> Result<Option<String>, DriverFailure>;
    fn default_endpoints(&mut self) -> Result<[Option<String>; 3], DriverFailure>;
    fn install_driver(&mut self) -> Result<(), DriverFailure>;
    fn wait_for_reconfiguration(&mut self, interval: Duration);
    fn is_steam_speaker_endpoint(&mut self, endpoint: &str) -> Result<bool, DriverFailure>;
    fn current_default(&mut self, role: DefaultRole) -> Result<Option<String>, DriverFailure>;
    fn set_default(&mut self, endpoint: &str, role: DefaultRole) -> Result<(), DriverFailure>;
}

/// Installs the driver only when its endpoint is absent, then repairs only
/// default roles that Windows moved to that endpoint during installation.
pub(crate) fn ensure_steam_driver<B: SteamDriverBackend>(
    backend: &mut B,
) -> Result<InstallOutcome, DriverFailure> {
    if let Some(endpoint) = backend.steam_speaker_endpoint()? {
        return Ok(InstallOutcome::AlreadyPresent { endpoint });
    }

    let original_defaults = backend.default_endpoints()?;
    backend.install_driver()?;
    let installed_endpoint = (0..ENDPOINT_ACTIVATION_MAX_POLLS).find_map(|_| {
        backend.wait_for_reconfiguration(ENDPOINT_ACTIVATION_POLL_INTERVAL);
        backend.steam_speaker_endpoint().ok().flatten()
    });
    let mut restored_roles = Vec::new();
    let mut restore_failures = Vec::new();

    for role in DefaultRole::ALL {
        let Some(original_endpoint) = original_defaults[role.index()].as_deref() else {
            continue;
        };
        match backend.current_default(role) {
            Ok(Some(current_endpoint)) if current_endpoint == original_endpoint => continue,
            Ok(Some(current_endpoint)) => {
                let points_to_installed_endpoint = match installed_endpoint.as_deref() {
                    Some(installed_endpoint) => current_endpoint == installed_endpoint,
                    None => match backend.is_steam_speaker_endpoint(&current_endpoint) {
                        Ok(is_steam_endpoint) => is_steam_endpoint,
                        Err(failure) => {
                            restore_failures.push(RoleRestoreFailure { role, failure });
                            continue;
                        }
                    },
                };
                if points_to_installed_endpoint {
                    match backend.current_default(role) {
                        Ok(Some(rechecked_endpoint)) if rechecked_endpoint == current_endpoint => {
                            match backend.set_default(original_endpoint, role) {
                                Ok(()) => restored_roles.push(role),
                                Err(failure) => {
                                    restore_failures.push(RoleRestoreFailure { role, failure });
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(failure) => {
                            restore_failures.push(RoleRestoreFailure { role, failure });
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(failure) => restore_failures.push(RoleRestoreFailure { role, failure }),
        }
    }

    match installed_endpoint {
        Some(endpoint) => Ok(InstallOutcome::Installed {
            endpoint,
            restored_roles,
            restore_failures,
        }),
        None => Ok(InstallOutcome::InstalledEndpointUnavailable {
            restored_roles,
            restore_failures,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_path_uses_the_steam_windows_ten_x64_inf() {
        let base = Path::new("fixture-common-files-x86");
        assert_eq!(
            steam_driver_inf_path(base),
            base.join("Steam")
                .join("drivers")
                .join("Windows10")
                .join("x64")
                .join("SteamStreamingSpeakers.inf")
        );
    }

    #[test]
    fn default_role_names_are_stable_status_values() {
        assert_eq!(DefaultRole::Console.name(), "console");
        assert_eq!(DefaultRole::Multimedia.name(), "multimedia");
        assert_eq!(DefaultRole::Communications.name(), "communications");
    }

    struct FakeBackend {
        steam_endpoint: Option<String>,
        installed_endpoint: Option<String>,
        defaults: [Option<String>; 3],
        install_moves: Vec<DefaultRole>,
        change_during_wait: Vec<(DefaultRole, Option<String>)>,
        change_after_identity: Vec<(DefaultRole, Option<String>)>,
        restore_failures: Vec<DefaultRole>,
        endpoint_activation_after_waits: usize,
        endpoint_never_activates: bool,
        reconfiguration_waits: usize,
        install_calls: usize,
        snapshots: usize,
        assignments: Vec<(DefaultRole, String)>,
        install_error: Option<DriverFailure>,
    }

    impl Default for FakeBackend {
        fn default() -> Self {
            Self {
                steam_endpoint: None,
                installed_endpoint: None,
                defaults: [None, None, None],
                install_moves: Vec::new(),
                change_during_wait: Vec::new(),
                change_after_identity: Vec::new(),
                restore_failures: Vec::new(),
                endpoint_activation_after_waits: 1,
                endpoint_never_activates: false,
                reconfiguration_waits: 0,
                install_calls: 0,
                snapshots: 0,
                assignments: Vec::new(),
                install_error: None,
            }
        }
    }

    impl SteamDriverBackend for FakeBackend {
        fn steam_speaker_endpoint(&mut self) -> Result<Option<String>, DriverFailure> {
            Ok(self.steam_endpoint.clone())
        }

        fn default_endpoints(&mut self) -> Result<[Option<String>; 3], DriverFailure> {
            self.snapshots += 1;
            Ok(self.defaults.clone())
        }

        fn install_driver(&mut self) -> Result<(), DriverFailure> {
            self.install_calls += 1;
            if let Some(error) = &self.install_error {
                return Err(error.clone());
            }
            self.installed_endpoint = Some("render.steam".into());
            for role in &self.install_moves {
                self.defaults[role.index()] = self.installed_endpoint.clone();
            }
            Ok(())
        }

        fn wait_for_reconfiguration(&mut self, _interval: Duration) {
            self.reconfiguration_waits += 1;
            for (role, endpoint) in &self.change_during_wait {
                self.defaults[role.index()] = endpoint.clone();
            }
            if !self.endpoint_never_activates
                && self.reconfiguration_waits >= self.endpoint_activation_after_waits
            {
                self.steam_endpoint = self.installed_endpoint.clone();
            }
        }

        fn is_steam_speaker_endpoint(&mut self, endpoint: &str) -> Result<bool, DriverFailure> {
            for (role, new_endpoint) in &self.change_after_identity {
                self.defaults[role.index()] = new_endpoint.clone();
            }
            Ok(self.installed_endpoint.as_deref() == Some(endpoint))
        }

        fn current_default(&mut self, role: DefaultRole) -> Result<Option<String>, DriverFailure> {
            Ok(self.defaults[role.index()].clone())
        }

        fn set_default(&mut self, endpoint: &str, role: DefaultRole) -> Result<(), DriverFailure> {
            if self.restore_failures.contains(&role) {
                return Err(DriverFailure::new("audio_route_change_failed"));
            }
            self.assignments.push((role, endpoint.to_owned()));
            self.defaults[role.index()] = Some(endpoint.to_owned());
            Ok(())
        }
    }

    #[test]
    fn existing_steam_endpoint_skips_install_and_default_snapshots() {
        let mut backend = FakeBackend {
            steam_endpoint: Some("render.steam".into()),
            ..FakeBackend::default()
        };

        assert_eq!(
            ensure_steam_driver(&mut backend),
            Ok(InstallOutcome::AlreadyPresent {
                endpoint: "render.steam".into()
            })
        );
        assert_eq!(backend.install_calls, 0);
        assert_eq!(backend.snapshots, 0);
        assert!(backend.assignments.is_empty());
    }

    #[test]
    fn restores_only_roles_windows_redirected_to_steam_speakers() {
        let mut backend = FakeBackend {
            defaults: [
                Some("render.console".into()),
                Some("render.multimedia".into()),
                Some("render.comms".into()),
            ],
            install_moves: vec![DefaultRole::Console, DefaultRole::Communications],
            ..FakeBackend::default()
        };

        let outcome = ensure_steam_driver(&mut backend).unwrap();

        assert_eq!(
            outcome,
            InstallOutcome::Installed {
                endpoint: "render.steam".into(),
                restored_roles: vec![DefaultRole::Console, DefaultRole::Communications],
                restore_failures: Vec::new(),
            }
        );
        assert_eq!(
            backend.assignments,
            [
                (DefaultRole::Console, "render.console".into()),
                (DefaultRole::Communications, "render.comms".into()),
            ]
        );
        assert_eq!(
            backend.defaults[DefaultRole::Multimedia.index()],
            Some("render.multimedia".into())
        );
    }

    #[test]
    fn a_concurrent_user_default_choice_wins_during_installation() {
        let mut backend = FakeBackend {
            defaults: [
                Some("render.console".into()),
                Some("render.multimedia".into()),
                Some("render.comms".into()),
            ],
            install_moves: DefaultRole::ALL.to_vec(),
            change_during_wait: vec![(DefaultRole::Multimedia, Some("render.user-choice".into()))],
            ..FakeBackend::default()
        };

        let outcome = ensure_steam_driver(&mut backend).unwrap();

        assert_eq!(
            outcome,
            InstallOutcome::Installed {
                endpoint: "render.steam".into(),
                restored_roles: vec![DefaultRole::Console, DefaultRole::Communications],
                restore_failures: Vec::new(),
            }
        );
        assert_eq!(
            backend.defaults[DefaultRole::Multimedia.index()],
            Some("render.user-choice".into())
        );
        assert!(
            !backend
                .assignments
                .iter()
                .any(|(role, _)| *role == DefaultRole::Multimedia)
        );
    }

    #[test]
    fn roles_without_a_preinstall_default_are_not_changed() {
        let mut backend = FakeBackend {
            defaults: [
                Some("render.console".into()),
                None,
                Some("render.comms".into()),
            ],
            install_moves: DefaultRole::ALL.to_vec(),
            ..FakeBackend::default()
        };

        let outcome = ensure_steam_driver(&mut backend).unwrap();

        assert_eq!(
            outcome,
            InstallOutcome::Installed {
                endpoint: "render.steam".into(),
                restored_roles: vec![DefaultRole::Console, DefaultRole::Communications],
                restore_failures: Vec::new(),
            }
        );
        assert!(
            !backend
                .assignments
                .iter()
                .any(|(role, _)| *role == DefaultRole::Multimedia)
        );
    }

    #[test]
    fn install_failure_does_not_attempt_any_default_route_change() {
        let mut backend = FakeBackend {
            defaults: [Some("render.console".into()), None, None],
            install_error: Some(DriverFailure::native("administrator_required", 5)),
            ..FakeBackend::default()
        };

        assert_eq!(
            ensure_steam_driver(&mut backend),
            Err(DriverFailure::native("administrator_required", 5))
        );
        assert_eq!(backend.install_calls, 1);
        assert!(backend.assignments.is_empty());
    }

    #[test]
    fn restoration_errors_are_reported_while_other_roles_continue() {
        let mut backend = FakeBackend {
            defaults: [
                Some("render.console".into()),
                Some("render.multimedia".into()),
                Some("render.comms".into()),
            ],
            install_moves: DefaultRole::ALL.to_vec(),
            restore_failures: vec![DefaultRole::Multimedia],
            ..FakeBackend::default()
        };

        let outcome = ensure_steam_driver(&mut backend).unwrap();

        assert_eq!(
            outcome,
            InstallOutcome::Installed {
                endpoint: "render.steam".into(),
                restored_roles: vec![DefaultRole::Console, DefaultRole::Communications],
                restore_failures: vec![RoleRestoreFailure {
                    role: DefaultRole::Multimedia,
                    failure: DriverFailure::new("audio_route_change_failed"),
                }],
            }
        );
        assert_eq!(
            backend.defaults[DefaultRole::Multimedia.index()],
            Some("render.steam".into())
        );
    }

    #[test]
    fn waits_for_delayed_endpoint_activation_before_restoring_roles() {
        let mut backend = FakeBackend {
            defaults: [
                Some("render.console".into()),
                Some("render.multimedia".into()),
                Some("render.comms".into()),
            ],
            install_moves: vec![DefaultRole::Console, DefaultRole::Communications],
            endpoint_activation_after_waits: 3,
            ..FakeBackend::default()
        };

        let outcome = ensure_steam_driver(&mut backend).unwrap();

        assert_eq!(
            outcome,
            InstallOutcome::Installed {
                endpoint: "render.steam".into(),
                restored_roles: vec![DefaultRole::Console, DefaultRole::Communications],
                restore_failures: Vec::new(),
            }
        );
        assert_eq!(backend.reconfiguration_waits, 3);
        assert_eq!(
            backend.assignments,
            [
                (DefaultRole::Console, "render.console".into()),
                (DefaultRole::Communications, "render.comms".into()),
            ]
        );
    }

    #[test]
    fn timeout_restores_roles_still_pointing_at_the_registered_steam_endpoint() {
        let mut backend = FakeBackend {
            defaults: [
                Some("render.console".into()),
                Some("render.multimedia".into()),
                Some("render.comms".into()),
            ],
            install_moves: vec![DefaultRole::Console, DefaultRole::Communications],
            endpoint_never_activates: true,
            ..FakeBackend::default()
        };

        let outcome = ensure_steam_driver(&mut backend).unwrap();

        assert_eq!(
            outcome,
            InstallOutcome::InstalledEndpointUnavailable {
                restored_roles: vec![DefaultRole::Console, DefaultRole::Communications],
                restore_failures: Vec::new(),
            }
        );
        assert_eq!(backend.reconfiguration_waits, ENDPOINT_ACTIVATION_MAX_POLLS);
        assert_eq!(
            backend.assignments,
            [
                (DefaultRole::Console, "render.console".into()),
                (DefaultRole::Communications, "render.comms".into()),
            ]
        );
    }

    #[test]
    fn timeout_preserves_user_choice_for_roles_no_longer_on_steam_endpoint() {
        let mut backend = FakeBackend {
            defaults: [
                Some("render.console".into()),
                Some("render.multimedia".into()),
                Some("render.comms".into()),
            ],
            install_moves: DefaultRole::ALL.to_vec(),
            change_during_wait: vec![(DefaultRole::Multimedia, Some("render.user-choice".into()))],
            endpoint_never_activates: true,
            ..FakeBackend::default()
        };

        let outcome = ensure_steam_driver(&mut backend).unwrap();

        assert_eq!(
            outcome,
            InstallOutcome::InstalledEndpointUnavailable {
                restored_roles: vec![DefaultRole::Console, DefaultRole::Communications],
                restore_failures: Vec::new(),
            }
        );
        assert_eq!(
            backend.defaults[DefaultRole::Multimedia.index()],
            Some("render.user-choice".into())
        );
        assert!(
            !backend
                .assignments
                .iter()
                .any(|(role, _)| *role == DefaultRole::Multimedia)
        );
    }

    #[test]
    fn timeout_rereads_role_after_identity_check_before_restoring() {
        let mut backend = FakeBackend {
            defaults: [
                Some("render.console".into()),
                Some("render.multimedia".into()),
                Some("render.comms".into()),
            ],
            install_moves: vec![DefaultRole::Console, DefaultRole::Communications],
            change_after_identity: vec![(DefaultRole::Console, Some("render.user-choice".into()))],
            endpoint_never_activates: true,
            ..FakeBackend::default()
        };

        let outcome = ensure_steam_driver(&mut backend).unwrap();

        assert_eq!(
            outcome,
            InstallOutcome::InstalledEndpointUnavailable {
                restored_roles: vec![DefaultRole::Communications],
                restore_failures: Vec::new(),
            }
        );
        assert_eq!(
            backend.defaults[DefaultRole::Console.index()],
            Some("render.user-choice".into())
        );
        assert_eq!(
            backend.assignments,
            [(DefaultRole::Communications, "render.comms".into())]
        );
    }
}
