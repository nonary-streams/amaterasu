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
pub mod options;
#[cfg(test)]
mod tests;

use super::default_extensions::{
    COORDINATOR_COMPONENT as SESSION_LIFECYCLE, PROVIDER_COMPONENT as LINUX_FRAME_LIMITER,
    REGISTRY_COMPONENT as EXTENSION_REGISTRY,
};
use super::{
    APPLICATION, AUDIO, AUDIO_ENCODE, AUTHENTICATION, CAPTURE_KMS, CAPTURE_PIPEWIRE, CAPTURE_VKMS,
    CATALOG, CONFIGURATION, CONTROL_TRANSPORT, ComponentDefinition, DISPLAY, ENCODE, HOST_METRICS,
    INPUT, LAUNCH, LIBRARY, LUTRIS, MANAGEMENT, MEDIA_TRANSPORT, NETWORK, NVHTTP, PAIRING, RTSP,
    RUNTIME, RUNTIME_ID, SESSION, SESSION_HISTORY, STATE, STEAM, STREAM_SESSION, VIDEO,
    VIRTUAL_DISPLAY, names,
};
use amaterasu_contracts::{Grants, RuntimeConfig};
use amaterasu_package::{PackageResult, create_private_directory};
use options::{LinuxSessionOptions, NetworkOption};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

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
        "amaterasu.capture-kms/1",
        "amaterasu.capture-pipewire/1",
        "amaterasu.capture-vkms/1",
        "amaterasu.encode/1",
    ],
    native_uses: &[
        "amaterasu.capture-kms/1",
        "amaterasu.capture-pipewire/1",
        "amaterasu.capture-vkms/1",
        "amaterasu.encode/1",
    ],
    dependencies: &[
        RUNTIME_ID,
        "amaterasu.capture-kms",
        "amaterasu.capture-pipewire",
        "amaterasu.capture-vkms",
        "amaterasu.encode",
    ],
    ..VIDEO
};

/// The optional processor is a separately packaged native provider. Its service
/// is bound only in the explicitly selected RTX HDR composition.
pub const RTX_HDR_COMPONENT: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.rtx-hdr",
    library: "amaterasu_rtx_hdr",
    provides: &["amaterasu.rtx-hdr/1", "nonary.rtx-hdr/1"],
    calls: &[
        "amaterasu.session-lifecycle/1",
        "amaterasu.foreground-context/1",
    ],
    publishes: &[],
    dependencies: &[RUNTIME_ID, "amaterasu.foreground-context"],
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
        "amaterasu.capture-kms/1",
        "amaterasu.capture-pipewire/1",
        "amaterasu.capture-vkms/1",
        "amaterasu.encode/1",
        "amaterasu.session-lifecycle/1",
    ],
    native_uses: &[
        "amaterasu.capture-kms/1",
        "amaterasu.capture-pipewire/1",
        "amaterasu.capture-vkms/1",
        "amaterasu.encode/1",
        "amaterasu.rtx-hdr/1",
    ],
    dependencies: &[
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
    ],
    dependencies: &[
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
    DISPLAY,
    LINUX_SESSION_LAUNCH,
    INPUT,
    VIRTUAL_DISPLAY,
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
            calls: &append_name::<{ $base.calls.len() + 1 }>($base.calls, "nonary.rtx-hdr/1"),
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
    DISPLAY,
    LINUX_SESSION_LAUNCH,
    INPUT,
    VIRTUAL_DISPLAY,
    CAPTURE_KMS,
    CAPTURE_PIPEWIRE,
    CAPTURE_VKMS,
    AUDIO,
    AUDIO_ENCODE,
    ENCODE,
    FOREGROUND_CONTEXT,
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
    DISPLAY,
    LINUX_SESSION_LAUNCH,
    INPUT,
    VIRTUAL_DISPLAY,
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
    if options.rtx_hdr_runtime_directory.is_some() && options.external_extension_composition {
        return Err("RTX HDR and external extension composition cannot be combined".into());
    }
    let mut policy = RuntimeConfig::default();
    let components = if options.rtx_hdr_runtime_directory.is_some() {
        RTX_HDR_COMPONENTS
    } else {
        COMPONENTS
    };
    for component in components {
        if options.external_extension_composition
            && [SESSION_LIFECYCLE.id, EXTENSION_REGISTRY.id].contains(&component.id)
        {
            continue;
        }
        if component.id != RUNTIME_ID {
            policy
                .principals
                .insert(component.id.into(), component.grants());
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
    if !options.external_extension_composition {
        super::default_extensions::install(
            &mut policy,
            &[include_str!(
                "../../../../modules/frame-limiter/metadata/linux.json"
            )],
            &directories.lifecycle,
        )?;
    }
    if let Some(runtime_directory) = &options.rtx_hdr_runtime_directory {
        enable_rtx_hdr(&mut policy, runtime_directory)?;
    }
    super::operation_apis::install(&mut policy)?;
    Ok(policy)
}

/// Prepare an explicit RTX HDR policy for a complete signed Linux session set.
/// The caller must obtain `runtime_directory` from trusted host configuration;
/// browser requests do not select paths. Conflicts leave the input unchanged.
pub fn enable_rtx_hdr(policy: &mut RuntimeConfig, runtime_directory: &Path) -> PackageResult<()> {
    use std::path::Component;
    if !runtime_directory.is_absolute()
        || runtime_directory
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err("RTX HDR runtime directory must be an absolute normal path".into());
    }
    let mut candidate = policy.clone();
    let capture = candidate
        .settings
        .get_mut("amaterasu.capture-vkms")
        .and_then(Value::as_object_mut)
        .ok_or("RTX HDR requires configured VKMS capture")?;
    let authority = capture.get("authority_channel").and_then(Value::as_bool) == Some(true);
    let helper = capture
        .get("frame_helper")
        .and_then(Value::as_str)
        .is_some_and(|path| !path.is_empty() && Path::new(path).is_absolute());
    if !authority && !helper {
        return Err("RTX HDR requires an approved VKMS frame helper or capture authority".into());
    }
    // Capture validates that the module was built with driver-read-leases when
    // require_lease is set. Matching helper protocol and hardware are checked
    // when the provider establishes capture; the frame path fails closed.
    capture.insert("gpu_snapshot".into(), json!(true));
    capture.insert("require_lease".into(), json!(true));
    let video_settings = candidate
        .settings
        .get_mut(VIDEO_COMPONENT_ID)
        .and_then(Value::as_object_mut)
        .ok_or("Linux session video settings missing")?;
    if video_settings
        .get("cpu_processor_service")
        .is_some_and(|value| !value.is_null())
        || video_settings
            .get("gpu_processor_service")
            .is_some_and(|value| value.as_str() != Some("amaterasu.rtx-hdr/1"))
    {
        return Err("video already selects another processor".into());
    }
    video_settings.insert("gpu_processor_service".into(), json!("amaterasu.rtx-hdr/1"));
    video_settings.insert("gpu_processor_opt_in".into(), json!(true));
    let video_grants = candidate
        .principals
        .get_mut(VIDEO_COMPONENT_ID)
        .ok_or("Linux session video principal missing")?;
    video_grants.native_use.insert("amaterasu.rtx-hdr/1".into());
    video_grants
        .call
        .insert("amaterasu.session-lifecycle/1".into());
    let desired_grants = RTX_HDR_COMPONENT.grants();
    if let Some(existing) = candidate.principals.get(RTX_HDR_COMPONENT.id) {
        if serde_json::to_value(existing).map_err(|error| error.to_string())?
            != serde_json::to_value(&desired_grants).map_err(|error| error.to_string())?
        {
            return Err("RTX HDR principal conflicts with the approved composition".into());
        }
    } else {
        candidate
            .principals
            .insert(RTX_HDR_COMPONENT.id.into(), desired_grants);
    }
    if let Some(settings) = candidate.settings.get_mut(RTX_HDR_COMPONENT.id) {
        if settings.get("runtime_directory") != Some(&json!(runtime_directory)) {
            return Err("RTX HDR runtime directory conflicts with existing settings".into());
        }
        settings
            .as_object_mut()
            .ok_or("RTX HDR settings must be an object")?
            .insert("require_verified_runtime".into(), json!(true));
    } else {
        candidate.settings.insert(
            RTX_HDR_COMPONENT.id.into(),
            json!({"runtime_directory": runtime_directory, "require_verified_runtime": true}),
        );
    }
    let provider_settings = candidate
        .settings
        .get_mut(RTX_HDR_COMPONENT.id)
        .and_then(Value::as_object_mut)
        .ok_or("RTX HDR settings must be an object")?;
    // The host admits two simultaneous clients. Keep the selected processor's
    // default admission capacity consistent, preserving explicit operator limits.
    provider_settings.entry("max_streams").or_insert(json!(2));
    let foreground_service = json!("amaterasu.foreground-context/1");
    if provider_settings
        .get("foreground_context_service")
        .is_some_and(|selected| *selected != foreground_service)
    {
        return Err("RTX HDR foreground provider conflicts with existing settings".into());
    }
    provider_settings.insert("foreground_context_service".into(), foreground_service);
    let foreground_settings = json!({"allowed_principals": [RTX_HDR_COMPONENT.id]});
    if candidate
        .settings
        .get(FOREGROUND_CONTEXT.id)
        .is_some_and(|existing| *existing != foreground_settings)
    {
        return Err("foreground provider settings conflict with existing settings".into());
    }
    candidate
        .settings
        .insert(FOREGROUND_CONTEXT.id.into(), foreground_settings);
    let foreground_grants = FOREGROUND_CONTEXT.grants();
    if let Some(existing) = candidate.principals.get(FOREGROUND_CONTEXT.id) {
        if serde_json::to_value(existing).map_err(|error| error.to_string())?
            != serde_json::to_value(&foreground_grants).map_err(|error| error.to_string())?
        {
            return Err("foreground provider principal conflicts with approved composition".into());
        }
    } else {
        candidate
            .principals
            .insert(FOREGROUND_CONTEXT.id.into(), foreground_grants);
    }
    for principal in [
        "amaterasu.session-lifecycle",
        "amaterasu.extension-registry",
        "amaterasu.management",
        "amaterasu.nvhttp",
        "amaterasu.rtsp",
        "amaterasu.stream-session",
        CONSOLE_ID,
    ] {
        let grants = candidate
            .principals
            .get_mut(principal)
            .ok_or("RTX HDR requires the complete Linux session principal set")?;
        grants.call.insert("nonary.rtx-hdr/1".into());
    }
    let stream = candidate
        .settings
        .get_mut("amaterasu.stream-session")
        .and_then(Value::as_object_mut)
        .ok_or("stream session settings missing")?;
    let plan = json!({"provider":"amaterasu.rtx-hdr", "control_service":"nonary.rtx-hdr/1", "processor_service":"amaterasu.rtx-hdr/1", "interface_minor":1, "omit_disabled":true});
    if stream
        .get("processing_plan")
        .is_some_and(|existing| !existing.is_null() && *existing != plan)
    {
        return Err("stream session already selects another processor".into());
    }
    stream.insert("processing_plan".into(), plan);
    let lifecycle = candidate
        .settings
        .get_mut("amaterasu.session-lifecycle")
        .and_then(Value::as_object_mut)
        .ok_or("session lifecycle settings missing")?;
    let plan = json!({"provider":"amaterasu.rtx-hdr", "service":"nonary.rtx-hdr/1", "processor_service":"amaterasu.rtx-hdr/1", "interface_minor":1, "source_mode_providers":[]});
    if lifecycle
        .get("processing_policy")
        .is_some_and(|existing| !existing.is_null() && *existing != plan)
    {
        return Err("session lifecycle already selects another processor".into());
    }
    lifecycle.insert("processing_policy".into(), plan);
    let operations = lifecycle
        .get_mut("operation_grants")
        .and_then(Value::as_object_mut)
        .and_then(|grants| grants.get_mut("amaterasu.stream-session"))
        .and_then(Value::as_array_mut)
        .ok_or("stream lifecycle operation grants missing")?;
    if !operations.contains(&json!("read_frozen_provider_settings")) {
        operations.push(json!("read_frozen_provider_settings"));
    }
    let grants = lifecycle
        .get_mut("operation_grants")
        .and_then(Value::as_object_mut)
        .ok_or("stream lifecycle operation grants missing")?;
    let video_operations = grants
        .entry(VIDEO_COMPONENT_ID)
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or("invalid video lifecycle operation grants")?;
    if !video_operations.contains(&json!("read_live_provider_settings")) {
        video_operations.push(json!("read_live_provider_settings"));
    }
    let trusted_callers = lifecycle
        .get_mut("trusted_callers")
        .and_then(Value::as_array_mut)
        .ok_or("stream lifecycle trusted callers missing")?;
    if !trusted_callers.contains(&json!(VIDEO_COMPONENT_ID)) {
        trusted_callers.push(json!(VIDEO_COMPONENT_ID));
    }
    let readers = lifecycle
        .entry("provider_settings_readers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("invalid provider settings readers")?;
    let readers = readers
        .entry("amaterasu.stream-session")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or("invalid stream settings readers")?;
    if !readers.contains(&json!("amaterasu.rtx-hdr")) {
        readers.push(json!("amaterasu.rtx-hdr"));
    }
    let video_readers = lifecycle
        .get_mut("provider_settings_readers")
        .and_then(Value::as_object_mut)
        .ok_or("invalid provider settings readers")?
        .entry(VIDEO_COMPONENT_ID)
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or("invalid video settings readers")?;
    if !video_readers.contains(&json!("amaterasu.rtx-hdr")) {
        video_readers.push(json!("amaterasu.rtx-hdr"));
    }
    super::default_extensions::add_extension(
        &mut candidate,
        include_str!("../../../../modules/rtx-hdr/metadata/linux.json"),
        Some("amaterasu.stream-session"),
    )?;
    *policy = candidate;
    Ok(())
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
    insert(
        "amaterasu.stream-session",
        json!({
            "enabled": true,
            "allow_remote": network.allow_remote(),
            "allowed_principals": ["amaterasu.rtsp"],
            "enable_input": options.enable_input,
        }),
    );
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
    let mut video_settings = if options.enable_virtual_screen_capture {
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
        video_settings["gpu_processor_service"] = json!("amaterasu.rtx-hdr/1");
        video_settings["gpu_processor_opt_in"] = json!(true);
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
        return json!({
            "enabled": options.enable_capture,
            "capture_principals": [VIDEO_COMPONENT_ID],
            "authority_channel": true,
            "gpu_snapshot": false,
        });
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
