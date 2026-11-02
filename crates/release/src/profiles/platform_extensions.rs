//! Default product assembly of the public lifecycle and settings contracts.
//! The selected provider supplies its own descriptor, schemas and behavior.
use super::default_extensions::{COORDINATOR_COMPONENT, PROVIDER_COMPONENT, REGISTRY_COMPONENT};
use super::*;

const SERVICES: &[&str] = &[
    "amaterasu.session-lifecycle/1",
    "amaterasu.extension-registry/1",
    "nonary.frame-limiter/1",
];

const fn append<const N: usize>(
    left: &[&'static str],
    right: &[&'static str],
) -> [&'static str; N] {
    assert!(N == left.len() + right.len());
    let mut result = [""; N];
    let mut index = 0;
    while index < left.len() {
        result[index] = left[index];
        index += 1;
    }
    let mut right_index = 0;
    while right_index < right.len() {
        result[index] = right[right_index];
        index += 1;
        right_index += 1;
    }
    result
}

pub(super) const COORDINATOR: ComponentDefinition = ComponentDefinition {
    calls: &append::<{ COORDINATOR_COMPONENT.calls.len() + 1 }>(
        COORDINATOR_COMPONENT.calls,
        &["amaterasu.host-state-context/1"],
    ),
    ..COORDINATOR_COMPONENT
};
pub(super) const REGISTRY: ComponentDefinition = REGISTRY_COMPONENT;
pub(super) const PROVIDER: ComponentDefinition = PROVIDER_COMPONENT;
pub(super) const TRANSPORT: ComponentDefinition = ComponentDefinition {
    calls: &append::<{ NVHTTP.calls.len() + 3 }>(NVHTTP.calls, SERVICES),
    dependencies: &append::<{ NVHTTP.dependencies.len() + 1 }>(
        NVHTTP.dependencies,
        &[COORDINATOR.id],
    ),
    ..NVHTTP
};
pub(super) const STREAM_CONTROL: ComponentDefinition = ComponentDefinition {
    calls: &append::<{ RTSP.calls.len() + 3 }>(RTSP.calls, SERVICES),
    dependencies: &append::<{ RTSP.dependencies.len() + 1 }>(RTSP.dependencies, &[COORDINATOR.id]),
    ..RTSP
};
pub(super) const STREAM: ComponentDefinition = ComponentDefinition {
    calls: &append::<{ STREAM_SESSION.calls.len() + 3 }>(STREAM_SESSION.calls, SERVICES),
    dependencies: &append::<{ STREAM_SESSION.dependencies.len() + 1 }>(
        STREAM_SESSION.dependencies,
        &[COORDINATOR.id],
    ),
    ..STREAM_SESSION
};
pub(super) const EXECUTOR: ComponentDefinition = ComponentDefinition {
    calls: &append::<{ LAUNCH.calls.len() + 1 }>(LAUNCH.calls, &["amaterasu.session-lifecycle/1"]),
    dependencies: &append::<{ LAUNCH.dependencies.len() + 1 }>(
        LAUNCH.dependencies,
        &[COORDINATOR.id],
    ),
    ..LAUNCH
};
pub(super) const ADMIN: ComponentDefinition = ComponentDefinition {
    calls: &append::<{ MANAGEMENT.calls.len() + 3 }>(MANAGEMENT.calls, SERVICES),
    dependencies: &append::<{ MANAGEMENT.dependencies.len() + 1 }>(
        MANAGEMENT.dependencies,
        &[COORDINATOR.id],
    ),
    ..MANAGEMENT
};
pub(super) const EXAMPLE_ADMIN: ComponentDefinition = ComponentDefinition {
    calls: &append::<{ MANAGEMENT_EXTENSION_EXAMPLE.calls.len() + 3 }>(
        MANAGEMENT_EXTENSION_EXAMPLE.calls,
        SERVICES,
    ),
    dependencies: &append::<{ MANAGEMENT_EXTENSION_EXAMPLE.dependencies.len() + 1 }>(
        MANAGEMENT_EXTENSION_EXAMPLE.dependencies,
        &[COORDINATOR.id],
    ),
    ..MANAGEMENT_EXTENSION_EXAMPLE
};

pub(super) const WINDOWS_METADATA: &str =
    include_str!("../../../../modules/frame-limiter/metadata/windows.json");
pub(super) const NATIVE_METADATA: &str = if cfg!(target_os = "linux") {
    include_str!("../../../../modules/frame-limiter/metadata/linux.json")
} else if cfg!(target_os = "macos") {
    include_str!("../../../../modules/frame-limiter/metadata/macos.json")
} else {
    WINDOWS_METADATA
};

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_contracts::{Command, Response};
    use amaterasu_runtime::Broker;

    #[test]
    fn default_product_graph_has_no_startup_cycle() {
        for profile in [PackageProfile::Platform, PackageProfile::ExtensionExample] {
            let mut remaining: Vec<_> = profile.components().iter().collect();
            let mut loaded = BTreeSet::new();
            while !remaining.is_empty() {
                let index = remaining
                    .iter()
                    .position(|component| {
                        component
                            .dependencies
                            .iter()
                            .all(|dependency| loaded.contains(dependency))
                    })
                    .expect("dependency cycle");
                assert!(loaded.insert(remaining.remove(index).id));
            }
        }
    }

    #[test]
    fn installer_selects_the_windows_descriptor_and_scoped_context_grant() {
        let mut policy = installer_policy("web", BTreeMap::new());
        let installed = &policy.settings[REGISTRY.id]["descriptors"][0]["descriptor"];
        let metadata: serde_json::Value = serde_json::from_str(WINDOWS_METADATA).unwrap();
        assert_eq!(installed, &metadata["descriptor"]);
        let settings = &policy.settings[COORDINATOR.id];
        assert_eq!(settings["capture_binding_mode"], "external");
        assert_eq!(settings["journal_in_runtime_state"], true);
        assert_eq!(settings["journal_path"], "");
        assert_eq!(
            settings["participants"][0]["settings"]["frame_limiter_enable"],
            false
        );
        assert_eq!(
            policy.settings[EXECUTOR.id]["prepared_context_service"],
            "amaterasu.session-lifecycle/1"
        );
        assert_eq!(
            policy.settings[TRANSPORT.id]["session_lifecycle_capture_mode"],
            "external"
        );
        policy.host_context.state_root = Some("/trusted/user-state".into());
        let mut broker = Broker::new(policy.clone()).unwrap();
        for component in [COORDINATOR, PROVIDER, REGISTRY] {
            for service in component.provides {
                if policy
                    .api_methods
                    .iter()
                    .any(|approval| approval.method.service == *service)
                {
                    continue; // Typed services are registered by their approved methods.
                }
                assert!(matches!(
                    broker.dispatch(
                        component.id,
                        Command::Register {
                            service: (*service).into()
                        }
                    ),
                    Response::Ok
                ));
            }
        }
        for approval in &policy.api_methods {
            assert!(matches!(
                broker.dispatch(
                    &approval.provider,
                    Command::RegisterApi {
                        method: approval.method.clone()
                    }
                ),
                Response::Ok
            ));
        }
        assert!(matches!(
            broker.dispatch(COORDINATOR.id, Command::GetHostContext {}),
            Response::Result { .. }
        ));
        for principal in [PROVIDER.id, REGISTRY.id, TRANSPORT.id] {
            assert!(matches!(
                broker.dispatch(principal, Command::GetHostContext {}),
                Response::Error { .. }
            ));
        }
        // Descriptors and approval data are ordinary contract configuration.
        assert_eq!(
            policy.settings[PROVIDER.id]["lifecycle_coordinator"]["principal"],
            COORDINATOR.id
        );
        assert!(!policy.principals.contains_key("amaterasu.console"));
        assert!(installed.get("options").is_some());
    }
}
