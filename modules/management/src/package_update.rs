//! Updates the whole running native set from the repository that supplied it.
//!
//! The console checks the running set's own source repository for a newer
//! set, downloads it into that repository's candidate cache and asks the host
//! to activate it. This module prepares and asks; it decides nothing about
//! trust. The host shell re-reads the approval, the catalog and its protected
//! policy allowlist, verifies every cached byte again, waits for streams to
//! end, and rolls back a set that does not start.
//!
//! No request names a repository, a publisher, a path or a package: each of
//! those comes from the host's selection and startup composition.
use amaterasu_module_sdk::RuntimeClient;
use amaterasu_module_sdk::network::Network;
use serde_json::{Value, json};
use std::path::PathBuf;

#[cfg(target_os = "linux")]
mod check;
#[cfg(target_os = "linux")]
mod download;
#[cfg(target_os = "linux")]
mod environment;
#[cfg(target_os = "linux")]
mod plan;
#[cfg(target_os = "linux")]
mod service;
#[cfg(target_os = "linux")]
mod switching_record;
#[cfg(target_os = "linux")]
mod transport;
mod versions;

pub(crate) use versions::VersionRequest;

#[cfg(all(test, target_os = "linux"))]
mod tests;

/// The catalog does not single out one newer set for this host.
#[cfg(target_os = "linux")]
const CANDIDATE_AMBIGUOUS: &str = "candidate_ambiguous";

/// How a worker ended without a result.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Failure {
    /// The console or a shutdown stopped it.
    Cancelled,
    /// The status reply reports this code as its `error`.
    Failed(&'static str),
}

/// A console request, as the route table names it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Action {
    Status,
    Check,
    Download,
    Cancel,
    Activate,
}

impl Action {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        [
            Self::Status,
            Self::Check,
            Self::Download,
            Self::Cancel,
            Self::Activate,
        ]
        .into_iter()
        .find(|action| action.name() == name)
    }

    /// The action name `routes::resolve` gives the request.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Status => "package_update_status",
            Self::Check => "package_update_check",
            Self::Download => "package_update_download",
            Self::Cancel => "package_update_cancel",
            Self::Activate => "package_update_activate",
        }
    }
}

/// The module's package-update service. Hosts that cannot activate a set
/// answer every request with the unsupported reply.
pub(crate) struct PackageUpdate {
    #[cfg(target_os = "linux")]
    service: Option<service::Service<Network>>,
}

impl PackageUpdate {
    /// `network` is the module's Network SDK provider, absent when management
    /// serves no console. `repository_directory` comes from startup settings.
    pub(crate) fn start(
        runtime: &RuntimeClient,
        repository_directory: Option<PathBuf>,
        network: Option<&Network>,
    ) -> Self {
        #[cfg(target_os = "linux")]
        {
            let service = network.map(|network| {
                let environment = environment::Environment::load(
                    repository_directory,
                    crate::package_root(runtime),
                    crate::package_status::running_digest(runtime),
                );
                service::Service::new(network.clone(), environment)
            });
            Self { service }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (runtime, repository_directory, network);
            Self {}
        }
    }

    /// Retires a finished worker. Called on every tick; never waits.
    pub(crate) fn advance(&mut self) {
        #[cfg(target_os = "linux")]
        if let Some(service) = &mut self.service {
            service.advance();
        }
    }

    /// Lets the service learn, shortly after start, whether the host takes
    /// restore requests. Called on every tick; it asks the runtime a bounded
    /// number of times and never waits.
    pub(crate) fn observe_host(&mut self, runtime: &RuntimeClient) {
        #[cfg(target_os = "linux")]
        if let Some(service) = &mut self.service {
            service.observe_host(&mut RuntimeHost(runtime));
        }
        #[cfg(not(target_os = "linux"))]
        let _ = runtime;
    }

    /// Close admission without interrupting a worker that is already running.
    pub(crate) fn prepare_shutdown(&mut self) {
        #[cfg(target_os = "linux")]
        if let Some(service) = &mut self.service {
            service.prepare_shutdown();
        }
    }

    /// Cancels the worker and reports whether it has returned. The host keeps
    /// this module loaded until this is true.
    pub(crate) fn quiesce(&mut self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.service.as_mut().is_none_or(service::Service::quiesce)
        }
        #[cfg(not(target_os = "linux"))]
        true
    }

    /// True while an update uses the repository cache or the host's one
    /// activation request, so another install must not start.
    pub(crate) fn in_progress(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.service
                .as_ref()
                .is_some_and(service::Service::in_progress)
        }
        #[cfg(not(target_os = "linux"))]
        false
    }

    /// Answers one console request. `other_install_active` is true while the
    /// RTX HDR install prepares or activates a set of its own.
    pub(crate) fn request(
        &mut self,
        action: Action,
        runtime: &RuntimeClient,
        other_install_active: bool,
    ) -> Value {
        #[cfg(target_os = "linux")]
        if let Some(service) = &mut self.service {
            return service.request(action, &mut RuntimeHost(runtime), other_install_active);
        }
        let _ = (runtime, other_install_active);
        unsupported(action)
    }
}

#[cfg(all(test, target_os = "linux"))]
impl PackageUpdate {
    /// An update whose activation request the host holds.
    pub(crate) fn awaiting_activation() -> Self {
        let mut service = service::Service::new(
            Network::test_provider(),
            environment::Environment::load(None, None, None),
        );
        service.assume_activation_requested();
        Self {
            service: Some(service),
        }
    }
}

impl PackageUpdate {
    /// Answers a request about the retained sets: the list of them, or a
    /// return to one. `other_install_active` is as for `request`.
    pub(crate) fn versions(
        &mut self,
        request: VersionRequest,
        runtime: &RuntimeClient,
        other_install_active: bool,
    ) -> Value {
        #[cfg(target_os = "linux")]
        if let Some(service) = &mut self.service {
            return service.versions(request, &mut RuntimeHost(runtime), other_install_active);
        }
        let _ = (runtime, other_install_active);
        match request {
            VersionRequest::List => versions::unsupported(),
            VersionRequest::Restore { .. } => json!({"ok": false, "code": "unsupported_platform"}),
        }
    }
}

/// The reply of a host that cannot activate a package: the status says so,
/// and every request that would change something is refused.
fn unsupported(action: Action) -> Value {
    if action != Action::Status {
        return json!({"ok": false, "code": "unsupported_platform"});
    }
    json!({
        "ok": true,
        "supported": false,
        "phase": "idle",
        "checked_unix_ms": null,
        "source": null,
        "running": {"digest": null, "sequence": null},
        "update": null,
        "transferred_bytes": 0,
        "error": "unsupported_platform",
        "restoring": null,
        "repository_behind": false,
    })
}

/// The host shell's activation endpoint, reached through the runtime broker.
#[cfg(target_os = "linux")]
struct RuntimeHost<'a>(&'a RuntimeClient);

#[cfg(target_os = "linux")]
impl service::ActivationHost for RuntimeHost<'_> {
    fn activate(
        &mut self,
        request: amaterasu_contracts::HostPackageActivation,
    ) -> service::Admission {
        use amaterasu_module_sdk::{Command, Response};
        match self.0.send(Command::HostPackageActivate { request }) {
            Ok(Response::Ok) => service::Admission::Accepted,
            Ok(Response::Error { code, .. }) if code == "activation_in_progress" => {
                service::Admission::InProgress
            }
            _ => service::Admission::Unavailable,
        }
    }

    fn restore(&mut self, request: amaterasu_contracts::HostPackageRestore) -> service::Admission {
        use amaterasu_module_sdk::{Command, Response};
        match self.0.send(Command::HostPackageRestore { request }) {
            Ok(Response::Ok) => service::Admission::Accepted,
            Ok(Response::Error { code, .. }) if code == "activation_in_progress" => {
                service::Admission::InProgress
            }
            _ => service::Admission::Unavailable,
        }
    }

    fn restore_supported(&mut self) -> bool {
        use amaterasu_module_sdk::{Command, Response};
        // A runtime or a host shell from before restore requests answers
        // with an error, and nothing is offered.
        matches!(
            self.0.send(Command::HostPackageRestoreStatus {}),
            Ok(Response::Ok)
        )
    }

    fn cancel(&mut self) -> bool {
        use amaterasu_module_sdk::{Command, Response};
        matches!(
            self.0.send(Command::HostPackageActivationCancel {}),
            Ok(Response::Ok)
        )
    }

    fn state(&mut self) -> Option<service::HostActivationState> {
        use amaterasu_module_sdk::{Command, Response};
        match self.0.send(Command::HostPackageActivationStatus {}) {
            Ok(Response::HostPackageActivationState { pending, started }) => {
                Some(service::HostActivationState { pending, started })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod reply_tests {
    use super::*;

    #[test]
    fn every_action_has_the_name_the_route_table_gives_it() {
        for (path, action) in [
            ("status", Action::Status),
            ("check", Action::Check),
            ("download", Action::Download),
            ("cancel", Action::Cancel),
            ("activate", Action::Activate),
        ] {
            let route = crate::routes::resolve(&format!("/v1/package-update/{path}")).unwrap();
            assert_eq!(route.action, action.name());
            assert_eq!(Action::parse(route.action), Some(action));
        }
        for unknown in ["status", "package_update", "package_update_install", ""] {
            assert_eq!(Action::parse(unknown), None);
        }
    }

    #[test]
    fn a_host_that_cannot_activate_reports_it_and_refuses_every_change() {
        let status = unsupported(Action::Status);
        assert_eq!(status["ok"], true);
        assert_eq!(status["supported"], false);
        assert_eq!(status["phase"], "idle");
        assert_eq!(status["error"], "unsupported_platform");
        assert_eq!(status["transferred_bytes"], 0);
        assert_eq!(status["repository_behind"], false);
        for absent in ["checked_unix_ms", "source", "update", "restoring"] {
            assert!(status[absent].is_null(), "{absent}");
        }
        assert!(status["running"]["digest"].is_null());
        for action in [
            Action::Check,
            Action::Download,
            Action::Cancel,
            Action::Activate,
        ] {
            assert_eq!(
                unsupported(action),
                json!({"ok": false, "code": "unsupported_platform"})
            );
        }
    }
}
