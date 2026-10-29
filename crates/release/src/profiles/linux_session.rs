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

use super::{
    APPLICATION, AUDIO, AUDIO_ENCODE, AUTHENTICATION, CAPTURE_KMS, CAPTURE_PIPEWIRE, CAPTURE_VKMS,
    CATALOG, CONFIGURATION, CONTROL_TRANSPORT, ComponentDefinition, DISPLAY, ENCODE, FRAME_LIMITER,
    HOST_METRICS, INPUT, LAUNCH, LUTRIS, MANAGEMENT, MEDIA_TRANSPORT, NVHTTP, PAIRING, RTSP,
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
/// development policy holds 4 and the installer policy 3. Advance this whenever
/// the administrator principals or the allowed scopes below
/// change, including a revert. TTL-only edits do not bump it.
pub const AUTHORIZATION_POLICY_GENERATION: u64 = 9;

pub const ADMINISTRATOR_PRINCIPALS: [&str; 1] = ["amaterasu.console"];
pub const PAIRING_ADMINISTRATOR_PRINCIPALS: [&str; 2] =
    ["amaterasu.console", "amaterasu.management"];

/// Management scopes without diagnostics; capture scopes authorize the
/// current user's explicit XDG Desktop Portal source selection.
pub const ALLOWED_SCOPES: [&str; 19] = [
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
        "amaterasu.lutris/1",
        "amaterasu.catalog/1",
        "amaterasu.state/1",
        "amaterasu.session/1",
        "amaterasu.application/1",
    ],
    dependencies: &[
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
/// outputs as DMA-BUF scanout through
/// capture-vkms when its frame helper is usable, and otherwise through a
/// current-user PipeWire stream (KWin ScreenCast or the portal).
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

// The grant chain. A parented request carries the intersection of every
// ancestor's call grants, so PipeWire call authority must transit NVHTTP,
// RTSP, and the stream session. Native-use authority stays on the Linux video
// component, which is the only module that directly invokes capture frames:
//   nvhttp -> rtsp -> stream-session -> video / media-transport / control-transport
//   video -> encode / current-user PipeWire capture
//   stream-session -> session / display / virtual-display / input / audio / audio-encode
//   nvhttp -> launch / catalog / application / session / input / pairing / encode
const LINUX_SESSION_NVHTTP: ComponentDefinition = ComponentDefinition {
    calls: &[
        "amaterasu.lutris/1",
        "amaterasu.configuration/1",
        "amaterasu.input/1",
        "amaterasu.pairing/1",
        "amaterasu.state/1",
        "amaterasu.catalog/1",
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
        RUNTIME_ID,
        "amaterasu.configuration",
        "amaterasu.pairing",
        "amaterasu.state",
        "amaterasu.catalog",
        "amaterasu.launch",
        "amaterasu.display",
        "amaterasu.rtsp",
    ],
    ..NVHTTP
};

const LINUX_SESSION_RTSP: ComponentDefinition = ComponentDefinition {
    calls: &[
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
        "amaterasu.frame-limiter/1",
    ],
    dependencies: &[
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
    ],
    dependencies: &[
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
    STATE,
    SESSION,
    CONFIGURATION,
    SESSION_HISTORY,
    HOST_METRICS,
    STEAM,
    LUTRIS,
    CATALOG,
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
    FRAME_LIMITER,
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
    let mut policy = RuntimeConfig::default();
    for component in COMPONENTS {
        if component.id != RUNTIME_ID {
            policy
                .principals
                .insert(component.id.into(), component.grants());
        }
    }
    policy.principals.insert(
        CONSOLE_ID.into(),
        Grants {
            call: names(CONSOLE_CALLS),
            ..Default::default()
        },
    );

    let directories = PrivateStateDirectories::create(root)?;
    policy.settings = settings(options, &directories);
    Ok(policy)
}

/// Persistent state locations, each private to the host user (mode 0700).
struct PrivateStateDirectories {
    state: std::path::PathBuf,
    session_history_database: std::path::PathBuf,
}

impl PrivateStateDirectories {
    fn create(root: &Path) -> PackageResult<Self> {
        let data = root.join("data");
        create_private_directory(&data)?;
        let state = canonical_private_directory(&data.join("state"))?;
        let history = canonical_private_directory(&state.join("session-history"))?;
        Ok(Self {
            state,
            session_history_database: history.join("history.db"),
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
        "amaterasu.pairing",
        json!({
            "administrator_principals": PAIRING_ADMINISTRATOR_PRINCIPALS,
            "transport_principals": ["amaterasu.nvhttp"],
        }),
    );
    insert("amaterasu.catalog", catalog_settings(options));

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
            "gpu_snapshot": true,
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
        "export_principals": {"amaterasu.nvhttp": owner},
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
            "capture_provider": if options.enable_virtual_screen_capture { "virtual_screen" } else if options.capture_output == options::DEFAULT_CAPTURE_OUTPUT { "pipewire" } else { "kms" },
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
