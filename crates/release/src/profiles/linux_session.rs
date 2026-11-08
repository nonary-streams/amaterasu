//! The Linux session product profile.
//!
//! This composition carries real input, audio, KMS and portal capture, virtual display
//! and encoder libraries so a Moonlight client can reach a Linux host. It is
//! separate from `LinuxProbe`, which holds qualification fixtures, and from
//! `Platform`, which is the Windows product. Every effect-bearing component
//! starts disabled and every listener binds loopback until the operator passes
//! an explicit option to `init-linux-session`.
//!
//! The updater, system tray, UPnP, host configuration, diagnostics, and
//! Playnite are not part of this composition. Leaving a component out is the
//! only reliable way to keep it disabled: the updater has no bootstrap-level
//! switch and would otherwise check a release feed on start.
mod display_settings;
pub mod options;
#[cfg(test)]
mod tests;

use super::default_extensions::{
    COORDINATOR_COMPONENT as SESSION_LIFECYCLE, PROVIDER_COMPONENT as LINUX_FRAME_LIMITER,
    REGISTRY_COMPONENT as EXTENSION_REGISTRY,
};
use super::remote_sessions::{BLACK_CAPTURE, LINUX_COMPONENT as REMOTE_SESSION, RETAINED_DISPLAY};
use super::{
    APPLICATION, AUDIO, AUDIO_ENCODE, AUTHENTICATION, CAPTURE_KMS, CAPTURE_PIPEWIRE, CAPTURE_VKMS,
    CATALOG, CONFIGURATION, CONTROL_TRANSPORT, ComponentDefinition, ENCODE, HOST_METRICS, INPUT,
    LAUNCH, LIBRARY, LUTRIS, MANAGEMENT, MEDIA_TRANSPORT, NETWORK, NVHTTP, PAIRING, RTSP, RUNTIME,
    RUNTIME_ID, SESSION, SESSION_HISTORY, STATE, STEAM, STREAM_SESSION, VIDEO, VIRTUAL_DISPLAY,
    names,
};
use amaterasu_contracts::{Grants, RuntimeConfig};
use amaterasu_package::{PackageResult, create_private_directory};
use options::{LinuxSessionOptions, NetworkOption};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

pub use amaterasu_module_sdk::processing_composition::{CapabilityBinding, ProcessingSelection};

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
            RTX_HDR_COMPONENT.id,
            "amaterasu.rtx-hdr/1",
            native_contracts::FRAME_PROCESSOR,
        ),
        frame_consumer: binding(
            VIDEO_COMPONENT_ID,
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
            SESSION_LIFECYCLE.id,
            extension_api::SESSION_LIFECYCLE_SERVICE,
            extension_api::SESSION_LIFECYCLE_SERVICE,
        )),
        settings_registry: Some(binding(
            EXTENSION_REGISTRY.id,
            extension_api::EXTENSION_REGISTRY_SERVICE,
            extension_api::EXTENSION_REGISTRY_SERVICE,
        )),
        display_settings: Some(binding(
            DISPLAY_SETTINGS.id,
            amaterasu_module_sdk::display_settings::SERVICE,
            amaterasu_module_sdk::display_settings::SERVICE,
        )),
        foreground: None,
        source_preflight_consumers: ["amaterasu.remote-session".into()].into(),
        request_ancestors: [
            "amaterasu.management",
            "amaterasu.nvhttp",
            "amaterasu.rtsp",
            CONSOLE_ID,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        settings_editors: ["amaterasu.management", CONSOLE_ID]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        prepared_context_consumers: ["amaterasu.launch".into()].into(),
        lifecycle_admitters: ["amaterasu.rtsp".into()].into(),
        history_owners: ["amaterasu.nvhttp".into()].into(),
    }
}

/// Authorization generations are profile-specific and never reused: the
/// development policy holds 13 and the installer policy 14. Advance this whenever
/// the administrator principals or the allowed scopes below
/// change, including a revert. TTL-only edits do not bump it.
pub const AUTHORIZATION_POLICY_GENERATION: u64 = 15;

pub const ADMINISTRATOR_PRINCIPALS: [&str; 1] = ["amaterasu.console"];
pub const PAIRING_ADMINISTRATOR_PRINCIPALS: [&str; 2] =
    ["amaterasu.console", "amaterasu.management"];

/// Management scopes without diagnostics; capture scopes authorize the
/// current user's explicit XDG Desktop Portal source selection.
pub const ALLOWED_SCOPES: [&str; 21] = [
    "account.self",
    "catalog.read",
    "catalog.write",
    "configuration.read",
    "configuration.write",
    "session.read",
    "session.write",
    "application.read",
    "application.launch",
    "application.terminate",
    "steam.read",
    "steam.refresh",
    "lutris.read",
    "host-logs.read",
    "tokens.manage",
    "pairing.admin",
    "display.read",
    "display.write",
    "capture.read",
    "capture.write",
    "media.read",
];

pub const VIDEO_COMPONENT_ID: &str = "amaterasu.linux-session-video";
pub const CONSOLE_ID: &str = "amaterasu.console";

/// Services the development console may call: pairing and catalog authoring,
/// listener status, and the state and configuration calls they delegate.
const CONSOLE_CALLS: &[&str] = &[
    "amaterasu.extension-registry/1",
    "amaterasu.session-lifecycle/1",
    "nonary.frame-limiter/1",
    "amaterasu.authentication/1",
    "amaterasu.catalog/1",
    "amaterasu.configuration/1",
    "amaterasu.nvhttp/1",
    "amaterasu.pairing/1",
    "amaterasu.session/1",
    "amaterasu.state/1",
];

/// Pairing without the tray notification or host configuration providers.
const LINUX_SESSION_PAIRING: ComponentDefinition = ComponentDefinition {
    calls: &["amaterasu.state/1"],
    dependencies: &[RUNTIME_ID, "amaterasu.state"],
    ..PAIRING
};

/// Launch without Playnite; Steam and Lutris integrations remain available.
const LINUX_SESSION_LAUNCH: ComponentDefinition = ComponentDefinition {
    calls: &[
        "amaterasu.session-lifecycle/1",
        "amaterasu.lutris/1",
        "amaterasu.catalog/1",
        "amaterasu.state/1",
        "amaterasu.session/1",
        "amaterasu.application/1",
    ],
    dependencies: &[
        "amaterasu.session-lifecycle",
        "amaterasu.lutris",
        RUNTIME_ID,
        "amaterasu.catalog",
        "amaterasu.state",
        "amaterasu.session",
        "amaterasu.application",
    ],
    ..LAUNCH
};

/// Video supports explicitly selected KMS outputs and captures owned virtual
/// outputs as DMA-BUF scanout through capture-vkms. PipeWire remains available
/// for explicitly selected current-user capture.
const LINUX_SESSION_VIDEO: ComponentDefinition = ComponentDefinition {
    id: VIDEO_COMPONENT_ID,
    calls: &[
        "amaterasu.capture-black/1",
        "amaterasu.capture-kms/1",
        "amaterasu.capture-pipewire/1",
        "amaterasu.capture-vkms/1",
        "amaterasu.encode/1",
    ],
    native_uses: &[
        "amaterasu.capture-black/1",
        "amaterasu.capture-kms/1",
        "amaterasu.capture-pipewire/1",
        "amaterasu.capture-vkms/1",
        "amaterasu.encode/1",
    ],
    dependencies: &[
        "amaterasu.capture-black",
        RUNTIME_ID,
        "amaterasu.capture-kms",
        "amaterasu.capture-pipewire",
        "amaterasu.capture-vkms",
        "amaterasu.encode",
    ],
    ..VIDEO
};

/// Default implementation of the provider-neutral Display Settings SDK.
pub const DISPLAY_SETTINGS: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.display-settings",
    library: "amaterasu_display_settings",
    provides: &["nonary.display-settings/1"],
    calls: &["amaterasu.host-state-context/1"],
    publishes: &[],
    dependencies: &[RUNTIME_ID],
    native_provides: &[],
    native_uses: &[],
};

/// The optional processor is a separately packaged native provider. Its service
/// is bound only in the explicitly selected RTX HDR composition.
pub const RTX_HDR_COMPONENT: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.rtx-hdr",
    library: "amaterasu_rtx_hdr",
    provides: &["amaterasu.rtx-hdr/1", "nonary.rtx-hdr/1"],
    calls: &["amaterasu.session-lifecycle/1", "nonary.display-settings/1"],
    publishes: &[],
    dependencies: &[RUNTIME_ID, "amaterasu.display-settings"],
    native_provides: &["amaterasu.rtx-hdr/1"],
    native_uses: &[],
};

const FOREGROUND_CONTEXT: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.foreground-context",
    library: "amaterasu_foreground_context",
    provides: &["amaterasu.foreground-context/1"],
    calls: &[],
    publishes: &[],
    dependencies: &[RUNTIME_ID],
    native_provides: &[],
    native_uses: &[],
};

const RTX_HDR_VIDEO: ComponentDefinition = ComponentDefinition {
    calls: &[
        "amaterasu.capture-black/1",
        "amaterasu.capture-kms/1",
        "amaterasu.capture-pipewire/1",
        "amaterasu.capture-vkms/1",
        "amaterasu.encode/1",
        "amaterasu.session-lifecycle/1",
    ],
    native_uses: &[
        "amaterasu.capture-black/1",
        "amaterasu.capture-kms/1",
        "amaterasu.capture-pipewire/1",
        "amaterasu.capture-vkms/1",
        "amaterasu.encode/1",
        "amaterasu.rtx-hdr/1",
    ],
    dependencies: &[
        "amaterasu.capture-black",
        RUNTIME_ID,
        "amaterasu.capture-kms",
        "amaterasu.capture-pipewire",
        "amaterasu.capture-vkms",
        "amaterasu.encode",
        "amaterasu.rtx-hdr",
        "amaterasu.session-lifecycle",
    ],
    ..LINUX_SESSION_VIDEO
};

// The grant chain. A parented request carries the intersection of every
// ancestor's call grants, so PipeWire call authority must transit NVHTTP,
// RTSP, and the stream session. Native-use authority stays on the Linux video
// component, which is the only module that directly invokes capture frames:
//   nvhttp -> rtsp -> stream-session -> video / media-transport / control-transport
//   video -> encode / current-user PipeWire capture
//   stream-session -> session / display / virtual-display / input / audio / audio-encode
//   nvhttp -> launch / catalog / application / session / input / pairing / encode
// The Linux session excludes Playnite, so only grant providers present in this profile.
const LINUX_SESSION_LIBRARY: ComponentDefinition = ComponentDefinition {
    native_uses: &["amaterasu.steam/1", "amaterasu.lutris/1"],
    calls: &[
        "amaterasu.catalog/1",
        "amaterasu.state/1",
        "amaterasu.steam/1",
        "amaterasu.lutris/1",
    ],
    dependencies: &[
        RUNTIME_ID,
        "amaterasu.catalog",
        "amaterasu.state",
        "amaterasu.steam",
        "amaterasu.lutris",
    ],
    ..LIBRARY
};

const LINUX_SESSION_NVHTTP: ComponentDefinition = ComponentDefinition {
    calls: &[
        "nonary.session-presentation/1",
        "amaterasu.capture-black/1",
        "amaterasu.host-package-barrier/1",
        "amaterasu.session-lifecycle/1",
        "amaterasu.extension-registry/1",
        "nonary.frame-limiter/1",
        "amaterasu.lutris/1",
        "amaterasu.configuration/1",
        "amaterasu.input/1",
        "amaterasu.pairing/1",
        "amaterasu.state/1",
        "amaterasu.catalog/1",
        "amaterasu.library/1",
        "amaterasu.steam/1",
        "amaterasu.launch/1",
        "amaterasu.display/1",
        "amaterasu.virtual-display/1",
        "amaterasu.application/1",
        "amaterasu.rtsp/1",
        "amaterasu.stream-session/1",
        "amaterasu.session/1",
        "amaterasu.video/1",
        "amaterasu.audio/1",
        "amaterasu.audio-encode/1",
        "amaterasu.media-transport/1",
        "amaterasu.control-transport/1",
        "amaterasu.capture-kms/1",
        "amaterasu.capture-pipewire/1",
        "amaterasu.capture-vkms/1",
        "amaterasu.encode/1",
    ],
    dependencies: &[
        "amaterasu.remote-session",
        "amaterasu.network",
        RUNTIME_ID,
        "amaterasu.configuration",
        "amaterasu.pairing",
        "amaterasu.state",
        "amaterasu.catalog",
        "amaterasu.library",
        "amaterasu.steam",
        "amaterasu.lutris",
        "amaterasu.launch",
        "amaterasu.display",
        "amaterasu.rtsp",
    ],
    ..NVHTTP
};

const LINUX_SESSION_RTSP: ComponentDefinition = ComponentDefinition {
    calls: &[
        "amaterasu.capture-black/1",
        "amaterasu.session-lifecycle/1",
        "amaterasu.extension-registry/1",
        "nonary.frame-limiter/1",
        "amaterasu.input/1",
        "amaterasu.stream-session/1",
        "amaterasu.session/1",
        "amaterasu.display/1",
        "amaterasu.virtual-display/1",
        "amaterasu.state/1",
        "amaterasu.launch/1",
        "amaterasu.video/1",
        "amaterasu.audio/1",
        "amaterasu.audio-encode/1",
        "amaterasu.media-transport/1",
        "amaterasu.control-transport/1",
        "amaterasu.capture-kms/1",
        "amaterasu.capture-pipewire/1",
        "amaterasu.capture-vkms/1",
        "amaterasu.encode/1",
    ],
    ..RTSP
};

const LINUX_SESSION_STREAM_SESSION: ComponentDefinition = ComponentDefinition {
    calls: &[
        "amaterasu.capture-black/1",
        "amaterasu.host-package-barrier/1",
        "amaterasu.session-lifecycle/1",
        "amaterasu.extension-registry/1",
        "nonary.frame-limiter/1",
        "amaterasu.host-metrics/1",
        "amaterasu.input/1",
        "amaterasu.session/1",
        "amaterasu.display/1",
        "amaterasu.virtual-display/1",
        "amaterasu.state/1",
        "amaterasu.video/1",
        "amaterasu.audio/1",
        "amaterasu.audio-encode/1",
        "amaterasu.media-transport/1",
        "amaterasu.control-transport/1",
        "amaterasu.capture-kms/1",
        "amaterasu.capture-pipewire/1",
        "amaterasu.capture-vkms/1",
        "amaterasu.encode/1",
        "amaterasu.session-history/1",
    ],
    dependencies: &[
        "amaterasu.session-lifecycle",
        "amaterasu.host-metrics",
        "amaterasu.input",
        RUNTIME_ID,
        "amaterasu.session",
        "amaterasu.display",
        "amaterasu.virtual-display",
        "amaterasu.state",
        "amaterasu.session-history",
        VIDEO_COMPONENT_ID,
        "amaterasu.audio",
        "amaterasu.audio-encode",
        "amaterasu.media-transport",
        "amaterasu.control-transport",
        "amaterasu.frame-limiter",
    ],
    ..STREAM_SESSION
};

const LINUX_SESSION_MANAGEMENT: ComponentDefinition = ComponentDefinition {
    calls: &[
        "nonary.remote-session/1",
        "nonary.stats/1",
        "nonary.stats-history/1",
        "nonary.stream-status/1",
        "amaterasu.host-package-activation/1",
        "amaterasu.extension-registry/1",
        "nonary.frame-limiter/1",
        "amaterasu.library/1",
        "amaterasu.host-logs/1",
        "amaterasu.steam/1",
        "amaterasu.host-metrics/1",
        "amaterasu.lutris/1",
        // Loopback `/healthz` drives the fixed status-only operation of this
        // component against its authentication, catalog and pairing dependencies.
        "amaterasu.management/1",
        "amaterasu.authentication/1",
        "amaterasu.state/1",
        "amaterasu.catalog/1",
        "amaterasu.configuration/1",
        "amaterasu.session/1",
        "amaterasu.application/1",
        "amaterasu.input/1",
        "amaterasu.pairing/1",
        "amaterasu.display/1",
        "amaterasu.launch/1",
        // Authenticated status and portal-selection delegation through video.
        // Retain the capture call grant for management -> video -> PipeWire
        // nested authority intersection; management does not depend on or
        // invoke the provider directly.
        "amaterasu.capture-kms/1",
        "amaterasu.capture-pipewire/1",
        "amaterasu.capture-vkms/1",
        "amaterasu.audio/1",
        "amaterasu.audio-encode/1",
        "amaterasu.encode/1",
        "amaterasu.video/1",
        "amaterasu.nvhttp/1",
        // NVHTTP applies base-port changes through RTSP, which preflights its
        // media and control UDP listeners under the same administrator request.
        "amaterasu.rtsp/1",
        "amaterasu.media-transport/1",
        "amaterasu.control-transport/1",
        "amaterasu.session-history/1",
        "amaterasu.stream-session/1",
        "nonary.display/1",
        "nonary.catalog/1",
        "nonary.host-settings/1",
    ],
    dependencies: &[
        "amaterasu.remote-session",
        "amaterasu.library",
        "amaterasu.network",
        "amaterasu.steam",
        "amaterasu.host-metrics",
        "amaterasu.lutris",
        RUNTIME_ID,
        "amaterasu.authentication",
        "amaterasu.state",
        "amaterasu.catalog",
        "amaterasu.configuration",
        "amaterasu.session",
        "amaterasu.application",
        "amaterasu.input",
        "amaterasu.pairing",
        "amaterasu.display",
        "amaterasu.launch",
        "amaterasu.audio",
        "amaterasu.audio-encode",
        "amaterasu.encode",
        VIDEO_COMPONENT_ID,
        "amaterasu.nvhttp",
        "amaterasu.rtsp",
        "amaterasu.media-transport",
        "amaterasu.control-transport",
        "amaterasu.session-history",
    ],
    ..MANAGEMENT
};

pub const COMPONENTS: &[ComponentDefinition] = &[
    RUNTIME,
    NETWORK,
    STATE,
    SESSION,
    CONFIGURATION,
    SESSION_LIFECYCLE,
    EXTENSION_REGISTRY,
    SESSION_HISTORY,
    HOST_METRICS,
    STEAM,
    LUTRIS,
    CATALOG,
    LINUX_SESSION_LIBRARY,
    APPLICATION,
    AUTHENTICATION,
    LINUX_SESSION_MANAGEMENT,
    LINUX_SESSION_PAIRING,
    LINUX_SESSION_NVHTTP,
    RETAINED_DISPLAY,
    LINUX_SESSION_LAUNCH,
    INPUT,
    VIRTUAL_DISPLAY,
    REMOTE_SESSION,
    BLACK_CAPTURE,
    CAPTURE_KMS,
    CAPTURE_PIPEWIRE,
    CAPTURE_VKMS,
    AUDIO,
    AUDIO_ENCODE,
    ENCODE,
    LINUX_SESSION_VIDEO,
    MEDIA_TRANSPORT,
    CONTROL_TRANSPORT,
    LINUX_FRAME_LIMITER,
    LINUX_SESSION_STREAM_SESSION,
    LINUX_SESSION_RTSP,
];

const fn append_name<const N: usize>(
    values: &'static [&'static str],
    extra: &'static str,
) -> [&'static str; N] {
    assert!(values.len() + 1 == N);
    let mut result = [""; N];
    let mut index = 0;
    while index < values.len() {
        result[index] = values[index];
        index += 1;
    }
    result[index] = extra;
    result
}

macro_rules! with_rtx_control {
    ($name:ident, $base:ident) => {
        const $name: ComponentDefinition = ComponentDefinition {
            calls: &append_name::<{ $base.calls.len() + 2 }>(
                &append_name::<{ $base.calls.len() + 1 }>($base.calls, "nonary.rtx-hdr/1"),
                "nonary.display-settings/1",
            ),
            dependencies: &append_name::<{ $base.dependencies.len() + 1 }>(
                $base.dependencies,
                "amaterasu.rtx-hdr",
            ),
            ..$base
        };
    };
}

with_rtx_control!(RTX_HDR_COORDINATOR, SESSION_LIFECYCLE);
with_rtx_control!(RTX_HDR_REGISTRY, EXTENSION_REGISTRY);
with_rtx_control!(RTX_HDR_MANAGEMENT, LINUX_SESSION_MANAGEMENT);
with_rtx_control!(RTX_HDR_NVHTTP, LINUX_SESSION_NVHTTP);
with_rtx_control!(RTX_HDR_RTSP, LINUX_SESSION_RTSP);
with_rtx_control!(RTX_HDR_STREAM_SESSION, LINUX_SESSION_STREAM_SESSION);

/// Publisher-signed complete set for the optional RTX HDR composition.
/// The standard Linux session package remains independent of this provider.
pub const RTX_HDR_COMPONENTS: &[ComponentDefinition] = &[
    RUNTIME,
    NETWORK,
    STATE,
    SESSION,
    CONFIGURATION,
    RTX_HDR_COORDINATOR,
    RTX_HDR_REGISTRY,
    SESSION_HISTORY,
    HOST_METRICS,
    STEAM,
    LUTRIS,
    CATALOG,
    LINUX_SESSION_LIBRARY,
    APPLICATION,
    AUTHENTICATION,
    RTX_HDR_MANAGEMENT,
    LINUX_SESSION_PAIRING,
    RTX_HDR_NVHTTP,
    RETAINED_DISPLAY,
    LINUX_SESSION_LAUNCH,
    INPUT,
    VIRTUAL_DISPLAY,
    REMOTE_SESSION,
    BLACK_CAPTURE,
    CAPTURE_KMS,
    CAPTURE_PIPEWIRE,
    CAPTURE_VKMS,
    AUDIO,
    AUDIO_ENCODE,
    ENCODE,
    FOREGROUND_CONTEXT,
    DISPLAY_SETTINGS,
    RTX_HDR_COMPONENT,
    RTX_HDR_VIDEO,
    MEDIA_TRANSPORT,
    CONTROL_TRANSPORT,
    LINUX_FRAME_LIMITER,
    RTX_HDR_STREAM_SESSION,
    RTX_HDR_RTSP,
];

const EXTENSION_ACTIONS: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.extension-actions",
    library: "amaterasu_extension_actions",
    provides: &["amaterasu.extension-actions/1"],
    calls: &["amaterasu.state/1"],
    publishes: &[],
    dependencies: &[RUNTIME_ID, "amaterasu.state"],
    native_provides: &[],
    native_uses: &[],
};

pub const EXTENSION_COMPONENTS: &[ComponentDefinition] = &[
    RUNTIME,
    NETWORK,
    STATE,
    SESSION,
    CONFIGURATION,
    SESSION_LIFECYCLE,
    EXTENSION_REGISTRY,
    EXTENSION_ACTIONS,
    SESSION_HISTORY,
    HOST_METRICS,
    STEAM,
    LUTRIS,
    CATALOG,
    LINUX_SESSION_LIBRARY,
    APPLICATION,
    AUTHENTICATION,
    LINUX_SESSION_MANAGEMENT,
    LINUX_SESSION_PAIRING,
    LINUX_SESSION_NVHTTP,
    RETAINED_DISPLAY,
    LINUX_SESSION_LAUNCH,
    INPUT,
    VIRTUAL_DISPLAY,
    REMOTE_SESSION,
    BLACK_CAPTURE,
    CAPTURE_KMS,
    CAPTURE_PIPEWIRE,
    CAPTURE_VKMS,
    AUDIO,
    AUDIO_ENCODE,
    ENCODE,
    LINUX_SESSION_VIDEO,
    MEDIA_TRANSPORT,
    CONTROL_TRANSPORT,
    LINUX_FRAME_LIMITER,
    LINUX_SESSION_STREAM_SESSION,
    LINUX_SESSION_RTSP,
];

/// Builds the bootstrap policy for a Linux session root, creating its private
/// state directories under `root`.
pub fn linux_session_policy(
    root: &Path,
    options: &LinuxSessionOptions,
) -> PackageResult<RuntimeConfig> {
    options.validate().map_err(|error| error.to_string())?;
    let external_rtx =
        options.rtx_hdr_runtime_directory.is_some() && options.external_extension_composition;
    let mut policy = RuntimeConfig::default();
    let components = if options.rtx_hdr_runtime_directory.is_some() {
        RTX_HDR_COMPONENTS
    } else {
        COMPONENTS
    };
    for component in components {
        if options.rtx_hdr_runtime_directory.is_some() && component.id == FOREGROUND_CONTEXT.id {
            continue;
        }
        if options.external_extension_composition
            && !external_rtx
            && [
                SESSION_LIFECYCLE.id,
                EXTENSION_REGISTRY.id,
                LINUX_FRAME_LIMITER.id,
            ]
            .contains(&component.id)
        {
            continue;
        }
        if external_rtx
            && [
                LINUX_FRAME_LIMITER.id,
                FOREGROUND_CONTEXT.id,
                DISPLAY_SETTINGS.id,
            ]
            .contains(&component.id)
        {
            continue;
        }
        if component.id != RUNTIME_ID {
            let mut grants = component.grants();
            if external_rtx && component.id == RTX_HDR_COMPONENT.id {
                grants
                    .call
                    .remove(amaterasu_module_sdk::foreground_context::SERVICE);
                grants
                    .call
                    .remove(amaterasu_module_sdk::display_settings::SERVICE);
            }
            policy.principals.insert(component.id.into(), grants);
        }
    }
    // The manifest requests the capability, but installed policy grants it
    // only after explicit host provisioning.
    if let Some(management) = policy.principals.get_mut("amaterasu.management") {
        management
            .call
            .remove(amaterasu_contracts::HOST_PACKAGE_ACTIVATION_SERVICE);
    }
    // The standalone VKMS provider still pins an SDK released before typed
    // registration. This explicit principal/service attestation lets it use
    // the same capture contract until its independent SDK pin is updated.
    policy
        .legacy_native_contracts
        .entry("amaterasu.capture-vkms".into())
        .or_default()
        .insert(
            "amaterasu.capture-vkms/1".into(),
            "amaterasu.video-capture.native/1".into(),
        );
    policy.principals.insert(
        CONSOLE_ID.into(),
        Grants {
            call: names(CONSOLE_CALLS),
            ..Default::default()
        },
    );

    let directories = PrivateStateDirectories::create(root)?;
    policy.settings = settings(options, &directories);
    if !options.external_extension_composition || external_rtx {
        let extensions: &[&str] = if external_rtx {
            &[]
        } else {
            &[include_str!(
                "../../../../modules/frame-limiter/metadata/linux.json"
            )]
        };
        super::default_extensions::install(&mut policy, extensions, &directories.lifecycle)?;
    }
    if let Some(runtime_directory) = &options.rtx_hdr_runtime_directory {
        if external_rtx {
            let mut selection = default_rtx_hdr_selection();
            selection.display_settings = None;
            enable_rtx_hdr_with_selection(&mut policy, runtime_directory, &selection)?;
        } else {
            enable_rtx_hdr(&mut policy, runtime_directory)?;
        }
    }
    super::remote_sessions::install(&mut policy)?;
    super::operation_apis::install(&mut policy)?;
    Ok(policy)
}

/// Prepare an explicit RTX HDR policy for a complete signed Linux session set.
/// The caller must obtain `runtime_directory` from trusted host configuration;
/// browser requests do not select paths. Conflicts leave the input unchanged.
pub fn enable_rtx_hdr(policy: &mut RuntimeConfig, runtime_directory: &Path) -> PackageResult<()> {
    enable_rtx_hdr_with_selection(policy, runtime_directory, &default_rtx_hdr_selection())
}

/// Select RTX HDR through the public processing composition. All provider
/// configuration except RTX HDR's own runtime settings stays provider-owned.
pub fn enable_rtx_hdr_with_selection(
    policy: &mut RuntimeConfig,
    runtime_directory: &Path,
    selection: &ProcessingSelection,
) -> PackageResult<()> {
    use std::path::Component;
    if !runtime_directory.is_absolute()
        || runtime_directory
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err("RTX HDR runtime directory must be an absolute normal path".into());
    }
    selection.validate().map_err(str::to_owned)?;
    if selection.processor.principal != RTX_HDR_COMPONENT.id
        || selection.processor.service != "amaterasu.rtx-hdr/1"
    {
        return Err("RTX HDR selection does not identify the RTX HDR processor".into());
    }
    // Reject selections that the built-in consumers would reject on startup.
    // External consumers own and validate their own configuration.
    if selection.frame_consumer.principal == VIDEO_COMPONENT_ID
        && let Some(video) = policy.settings.get(VIDEO_COMPONENT_ID)
        && ["cpu_processor_service", "resource_processor_service"]
            .iter()
            .any(|field| video.get(*field).is_some_and(|value| !value.is_null()))
    {
        return Err("video already selects a different processor".into());
    }
    if let (Some(owner), Some(display)) = (&selection.stream_owner, &selection.display_settings)
        && owner.principal == "amaterasu.stream-session"
        && let Some(existing) = policy
            .settings
            .get(&owner.principal)
            .and_then(|settings| settings.get("source_preparation"))
            .and_then(|preparation| preparation.get("display_settings_service"))
            .and_then(serde_json::Value::as_str)
        && existing != display.service
    {
        return Err("display preparation selection conflicts".into());
    }
    let mut candidate = policy.clone();
    let mut required = RTX_HDR_COMPONENT.grants();
    required.call.clear();
    if let Some(existing) = candidate.principals.get(RTX_HDR_COMPONENT.id) {
        if !contains_grants(existing, &required) {
            return Err("RTX HDR provider conflicts with approved composition".into());
        }
    } else {
        candidate
            .principals
            .insert(RTX_HDR_COMPONENT.id.into(), required);
    }
    let settings = candidate
        .settings
        .entry(RTX_HDR_COMPONENT.id.into())
        .or_insert_with(|| json!({"runtime_directory": runtime_directory}));
    if settings.get("runtime_directory") != Some(&json!(runtime_directory)) {
        return Err("RTX HDR runtime directory conflicts with existing settings".into());
    }
    let settings = settings
        .as_object_mut()
        .ok_or("RTX HDR settings must be an object")?;
    settings.insert("require_verified_runtime".into(), json!(true));
    settings.entry("max_streams").or_insert(json!(2));
    if let Some(display) = &selection.display_settings
        && display.principal == DISPLAY_SETTINGS.id
        && display.service == amaterasu_module_sdk::display_settings::SERVICE
    {
        display_settings::install_default_provider(&mut candidate, selection)?;
    }
    if let Some(foreground) = &selection.foreground {
        configure_foreground_provider(&mut candidate, foreground)?;
    }
    super::processing::install(
        &mut candidate,
        selection,
        include_str!("../../../../modules/rtx-hdr/metadata/linux.json"),
    )?;
    *policy = candidate;
    Ok(())
}

fn configure_foreground_provider(
    policy: &mut RuntimeConfig,
    selected: &CapabilityBinding,
) -> PackageResult<()> {
    if selected.principal == FOREGROUND_CONTEXT.id
        && selected.service == amaterasu_module_sdk::foreground_context::SERVICE
    {
        let settings = policy
            .settings
            .entry(FOREGROUND_CONTEXT.id.into())
            .or_insert_with(|| json!({"allowed_principals": []}))
            .as_object_mut()
            .ok_or("foreground provider settings conflict with existing settings")?;
        let principals = settings
            .entry("allowed_principals")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("foreground provider settings conflict with existing settings")?;
        if !principals.contains(&json!(RTX_HDR_COMPONENT.id)) {
            principals.push(json!(RTX_HDR_COMPONENT.id));
        }
        let grants = FOREGROUND_CONTEXT.grants();
        if let Some(existing) = policy.principals.get(FOREGROUND_CONTEXT.id) {
            if !contains_grants(existing, &grants) {
                return Err(
                    "foreground provider principal conflicts with approved composition".into(),
                );
            }
        } else {
            policy
                .principals
                .insert(FOREGROUND_CONTEXT.id.into(), grants);
        }
    }
    if !policy
        .principals
        .get(&selected.principal)
        .is_some_and(|grants| grants.provide.contains(&selected.service))
    {
        return Err("selected foreground provider does not provide its service".into());
    }
    Ok(())
}

fn contains_grants(existing: &Grants, required: &Grants) -> bool {
    required.native_provide.is_subset(&existing.native_provide)
        && required.native_use.is_subset(&existing.native_use)
        && required.provide.is_subset(&existing.provide)
        && required.call.is_subset(&existing.call)
        && required.publish.is_subset(&existing.publish)
        && required.subscribe.is_subset(&existing.subscribe)
        && required.hook.is_subset(&existing.hook)
}

/// Persistent state locations, each private to the host user (mode 0700).
struct PrivateStateDirectories {
    state: std::path::PathBuf,
    session_history_database: std::path::PathBuf,
    lifecycle: std::path::PathBuf,
    rtx_hdr_runtime_parent: std::path::PathBuf,
}

impl PrivateStateDirectories {
    fn create(root: &Path) -> PackageResult<Self> {
        let data = root.join("data");
        create_private_directory(&data)?;
        let state = canonical_private_directory(&data.join("state"))?;
        let history = canonical_private_directory(&state.join("session-history"))?;
        let lifecycle = canonical_private_directory(&state.join("session-lifecycle"))?;
        let rtx_hdr_runtime_parent = canonical_private_directory(&state.join("rtx-hdr"))?;
        Ok(Self {
            state,
            session_history_database: history.join("history.db"),
            lifecycle,
            rtx_hdr_runtime_parent,
        })
    }
}

fn canonical_private_directory(path: &Path) -> PackageResult<std::path::PathBuf> {
    create_private_directory(path)?;
    path.canonicalize().map_err(|error| error.to_string())
}

fn settings(
    options: &LinuxSessionOptions,
    directories: &PrivateStateDirectories,
) -> BTreeMap<String, Value> {
    let network = &options.network;
    let mut settings = BTreeMap::new();
    let mut insert = |component: &str, value: Value| {
        settings.insert(component.to_owned(), value);
    };

    insert(
        "amaterasu.state",
        json!({"data_directory": directories.state}),
    );
    insert(
        "amaterasu.session-history",
        json!({"database_path": directories.session_history_database.to_string_lossy()}),
    );
    insert("amaterasu.authentication", authentication_settings());
    insert(
        "amaterasu.management",
        json!({
            "rtx_hdr_runtime_parent": directories.rtx_hdr_runtime_parent,
        }),
    );
    insert(
        "amaterasu.pairing",
        json!({
            "administrator_principals": PAIRING_ADMINISTRATOR_PRINCIPALS,
            "transport_principals": ["amaterasu.nvhttp"],
        }),
    );
    insert("amaterasu.catalog", catalog_settings(options));
    insert(
        "amaterasu.launch",
        json!({"inherit_environment":[
            "HOME", "USER", "LOGNAME", "PATH", "DISPLAY", "WAYLAND_DISPLAY",
            "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "XAUTHORITY",
            "XDG_SESSION_TYPE", "LANG", "LC_ALL"
        ]}),
    );

    insert("amaterasu.nvhttp", nvhttp_settings(options));
    insert(
        "amaterasu.rtsp",
        json!({
            "enabled": true,
            "allow_remote": network.allow_remote(),
            "bind": network.rtsp_bind().to_string(),
            "address_family": network.address_family().setting(),
            "bind_address": network.bind_address(),
            "allowed_principals": ["amaterasu.nvhttp"],
        }),
    );
    let mut stream_settings = json!({
        "enabled": true,
        "allow_remote": network.allow_remote(),
        "allowed_principals": ["amaterasu.rtsp"],
        "enable_input": options.enable_input,
    });
    if !options.external_extension_composition {
        stream_settings["providers"] = json!({"lifecycle":"amaterasu.session-lifecycle/1"});
        stream_settings["lifecycle_admitters"] = json!(["amaterasu.rtsp"]);
        stream_settings["history_owners"] = json!(["amaterasu.nvhttp"]);
    }
    insert("amaterasu.stream-session", stream_settings);
    for transport in ["amaterasu.media-transport", "amaterasu.control-transport"] {
        insert(
            transport,
            json!({
                "enabled": true,
                "allow_remote": network.allow_remote(),
                "allowed_principals": ["amaterasu.stream-session"],
            }),
        );
    }

    // Software x264 and any hardware encoder only compute; they open no
    // display, audio, or input device. Enumeration feeds the codec capability
    // snapshot a client reads before it can start a stream.
    insert("amaterasu.encode", json!({"enabled": true}));
    insert("amaterasu.audio-encode", json!({"enabled": true}));

    // The portal owns the explicit user-consent prompt. Capture remains
    // disabled until selected in the portal; video processing is separately
    // enabled through the profile option.
    let video_settings = if options.enable_virtual_screen_capture {
        json!({
            "enabled": options.enable_capture,
            "capture_providers": ["kms", "pipewire"],
            "external_capture_providers": {"virtual_screen": {
                "service": "amaterasu.capture-vkms/1", "frame_transport": "dmabuf"
            }}
        })
    } else {
        json!({"enabled": options.enable_capture, "capture_providers": ["kms", "pipewire", "vkms"]})
    };
    if let Some(runtime_directory) = &options.rtx_hdr_runtime_directory {
        insert(
            "amaterasu.rtx-hdr",
            json!({"runtime_directory": runtime_directory}),
        );
    }
    insert(VIDEO_COMPONENT_ID, video_settings);
    let mut capture_settings = portal_capture_settings();
    if options.enable_virtual_display {
        capture_settings["allow_named_outputs"] = json!(true);
    }
    if options.enable_desktop_output_capture {
        capture_settings["allow_desktop_outputs"] = json!(true);
    }
    insert("amaterasu.capture-pipewire", capture_settings);
    insert("amaterasu.capture-vkms", vkms_capture_settings(options));
    insert(
        "amaterasu.capture-kms",
        json!({"enabled": options.enable_capture}),
    );
    insert("amaterasu.audio", json!({"enabled": options.enable_audio}));
    // Owned displays are the only screen while streaming (Vibeshine's default
    // exclusive layout) and are scaled to keep about 864 logical rows, since
    // KWin otherwise remembers a 100% scale for these connectors.
    insert(
        "amaterasu.virtual-display",
        json!({
            "enabled": options.enable_virtual_display,
            "virtual_display_layout": "exclusive",
            "dd_virtual_display_scale": -1,
        }),
    );
    insert(
        "amaterasu.input",
        json!({"allow_injection": options.enable_input}),
    );
    insert(
        "amaterasu.display",
        json!({"allow_mutation": options.enable_display_mutation}),
    );
    settings
}

fn authentication_settings() -> Value {
    json!({
        "administrator_principals": ADMINISTRATOR_PRINCIPALS,
        "legacy_verifier_principals": ["amaterasu.management"],
        "authorization_policy_generation": AUTHORIZATION_POLICY_GENERATION,
        "session_token_ttl_seconds": 7200,
        "remember_me_refresh_token_ttl_seconds": 604800,
        "allowed_scopes": ALLOWED_SCOPES,
    })
}

fn portal_capture_settings() -> Value {
    json!({
        "enabled": true,
        "selection_principals": [VIDEO_COMPONENT_ID, MANAGEMENT.id],
        "capture_principals": [VIDEO_COMPONENT_ID],
    })
}

/// Package authority selects the broker-inherited channel; it never names a
/// descriptor or executable in user-controlled settings. The older helper
/// remains a separate explicit compatibility mode.
fn vkms_capture_settings(options: &LinuxSessionOptions) -> Value {
    if options.capture_authority_helper.is_some() {
        let mut settings = json!({
            "enabled": options.enable_capture,
            "capture_principals": [VIDEO_COMPONENT_ID],
            "authority_channel": true,
            "gpu_snapshot": false,
        });
        if options.capture_authority_precheck {
            // Reserved false placeholder: only the verified v2 root-peer
            // launcher may inject true. Older profiles omit the field.
            settings["authority_precheck_supported"] = json!(false);
        }
        return settings;
    }
    match &options.frame_helper {
        Some(helper) => json!({
            "enabled": options.enable_capture,
            "capture_principals": [VIDEO_COMPONENT_ID],
            "frame_helper": helper,
        }),
        None => json!({"enabled": false}),
    }
}

fn catalog_settings(options: &LinuxSessionOptions) -> Value {
    let owner = options.catalog_owner.principal();
    json!({
        "launch_principals": ["amaterasu.launch"],
        // NVHTTP advertises the reserved Desktop app ID directly. Do not also
        // create an ordinary catalog row named Desktop.
        "seed_desktop_entry": false,
        "steam_id_export_principals": {"amaterasu.lutris": owner},
        "import_principals": {"amaterasu.steam": owner, "amaterasu.lutris": owner},
        "export_principals": {"amaterasu.nvhttp": owner, "amaterasu.management": owner},
    })
}

fn nvhttp_settings(options: &LinuxSessionOptions) -> Value {
    let network: &NetworkOption = &options.network;
    json!({
        "enabled": true,
        "http_bind": network.http_bind().to_string(),
        "https_bind": network.https_bind().to_string(),
        "address_family": network.address_family().setting(),
        "bind_address": network.bind_address(),
        "allow_remote": network.allow_remote(),
        "streaming": streaming_settings(options),
    })
}

/// Streams reach 3840x2160 at 120 Hz when a hardware encoder serves the
/// negotiated codec. NVHTTP refuses a launch above 1920x1080 at 60 Hz that only
/// a software encoder could serve: software x264 cannot sustain the host
/// desktop's own 3024x1890 at 90 Hz, and such requests are refused instead of
/// silently degraded.
fn streaming_settings(options: &LinuxSessionOptions) -> Value {
    json!({
        "capture": {
            "capture_provider": if options.capture_output != options::DEFAULT_CAPTURE_OUTPUT { "kms" } else if options.enable_virtual_screen_capture { "virtual_screen" } else if options.enable_virtual_display { "vkms" } else { "pipewire" },
            "output": if options.capture_output != options::DEFAULT_CAPTURE_OUTPUT { options.capture_output.as_str() } else if options.enable_virtual_display { "amaterasu-owned-display" } else { "portal-selected-session-source" },
            // Automatic selection lets the video module open a hardware
            // encoder for whichever codec the client negotiates, then the
            // software encoder. A numeric index would pin one codec.
            "encoder": "auto",
            "audio_endpoint": options.audio_endpoint,
        },
        "profile": {
            "max_width": 3840,
            "max_height": 2160,
            "max_fps": 120,
            // A ceiling on the client's requested bitrate, which Moonlight
            // scales with the mode (about 64 Mbps by default at 4K60).
            "bitrate_kbps": 100_000,
            "max_reference_frames": 1,
            "packet_size": 1024,
            "fec_percentage": 20,
            "min_parity_shards": 2,
            "max_fec_blocks": 1,
            "packet_ms": 10,
            "audio_bitrate": 128_000,
            "encrypt_video": true,
            "encrypt_audio": true,
            // Moonlight derives its legacy audio IV from the wrapping RTP
            // sequence. Continue across that wrap instead of ending normal
            // sessions after 65536 audio packets.
            "permit_legacy_audio_iv_wrap": true,
        },
        "desktop": {"enabled": true},
    })
}

/// The management listener setting, applied by root initialisation because the
/// web directory and trusted keys it also carries belong to the root.
pub fn apply_management_bind(management: &mut serde_json::Map<String, Value>, port: Option<u16>) {
    if let Some(port) = port {
        management.insert("enabled".into(), json!(true));
        management.insert("bind".into(), json!(format!("127.0.0.1:{port}")));
    }
}
