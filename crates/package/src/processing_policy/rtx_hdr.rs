//! Built-in Linux RTX HDR profile shared by Core and the legacy complete set.
use amaterasu_contracts::{
    RuntimeConfig,
    processing_composition::{CapabilityBinding, ProcessingSelection},
};

pub const PROVIDER: &str = "amaterasu.rtx-hdr";
pub const DISPLAY: &str = "amaterasu.display-settings";
pub const FOREGROUND: &str = "amaterasu.foreground-context";
const VIDEO: &str = "amaterasu.linux-session-video";

/// The standard Linux product explicitly selects its built-in providers.
/// Other compositions can select different principals and service aliases.
pub fn default_rtx_hdr_selection() -> ProcessingSelection {
    use amaterasu_module_sdk::{extension_api, media::native_contracts};
    let binding = |principal: &str, service: &str, contract: &str| CapabilityBinding {
        principal: principal.into(),
        service: service.into(),
        contract: contract.into(),
        interface_major: 1,
        interface_minor: 0,
    };
    ProcessingSelection {
        processor: binding(
            PROVIDER,
            "amaterasu.rtx-hdr/1",
            native_contracts::FRAME_PROCESSOR,
        ),
        frame_consumer: binding(
            VIDEO,
            "amaterasu.video/1",
            amaterasu_module_sdk::video::CONTRACT,
        ),
        frame_settings: None,
        stream_owner: Some(binding(
            "amaterasu.stream-session",
            "amaterasu.stream-session/1",
            "amaterasu.stream-session/1",
        )),
        lifecycle: Some(binding(
            "amaterasu.session-lifecycle",
            extension_api::SESSION_LIFECYCLE_SERVICE,
            extension_api::SESSION_LIFECYCLE_SERVICE,
        )),
        settings_registry: Some(binding(
            "amaterasu.extension-registry",
            extension_api::EXTENSION_REGISTRY_SERVICE,
            extension_api::EXTENSION_REGISTRY_SERVICE,
        )),
        display_settings: Some(binding(
            DISPLAY,
            amaterasu_module_sdk::display_settings::SERVICE,
            amaterasu_module_sdk::display_settings::SERVICE,
        )),
        foreground: None,
        source_preflight_consumers: ["amaterasu.remote-session".into()].into(),
        request_ancestors: [
            "amaterasu.management",
            "amaterasu.nvhttp",
            "amaterasu.rtsp",
            "amaterasu.console",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        settings_editors: ["amaterasu.management", "amaterasu.console"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        prepared_context_consumers: ["amaterasu.launch".into()].into(),
        lifecycle_admitters: ["amaterasu.rtsp".into()].into(),
        history_owners: ["amaterasu.nvhttp".into()].into(),
    }
}

/// The standard selection for an installed policy, which may hold fewer
/// components than the current profile. Optional consumers it does not approve
/// are left out. Foreground restrictions need the built-in foreground provider,
/// so it is bound exactly when this policy approves that component; a persisted
/// foreground-only setting then has a provider to reach instead of failing
/// stream configuration.
pub fn default_rtx_hdr_selection_for(policy: &RuntimeConfig) -> ProcessingSelection {
    let mut selection = default_rtx_hdr_selection();
    selection
        .source_preflight_consumers
        .retain(|principal| policy.principals.contains_key(principal));
    let service = amaterasu_module_sdk::foreground_context::SERVICE;
    let approved = policy
        .principals
        .get(FOREGROUND)
        .is_some_and(|grants| grants.provide.contains(service));
    if approved {
        selection.foreground = Some(CapabilityBinding {
            principal: FOREGROUND.into(),
            service: service.into(),
            contract: service.into(),
            interface_major: 1,
            interface_minor: 0,
        });
    }
    selection
}
