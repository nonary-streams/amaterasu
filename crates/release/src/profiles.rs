//! Development package compositions. The host has no knowledge of these services.
use amaterasu_contracts::{Grants, RuntimeConfig};
use amaterasu_package::{PackageResult, create_private_directory};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const RUNTIME_ID: &str = "amaterasu.runtime";

#[derive(Clone, Copy)]
pub enum PackageProfile {
    Example,
    Platform,
    MediaProbe,
    StreamProbe,
    DesktopStreamSmoke,
}

#[derive(Clone, Copy)]
pub struct ComponentDefinition {
    pub id: &'static str,
    pub library: &'static str,
    pub provides: &'static [&'static str],
    pub calls: &'static [&'static str],
    pub publishes: &'static [&'static str],
    pub dependencies: &'static [&'static str],
    pub native_provides: &'static [&'static str],
    pub native_uses: &'static [&'static str],
}

const RUNTIME: ComponentDefinition = ComponentDefinition {
    id: RUNTIME_ID,
    library: "amaterasu_runtime",
    provides: &[],
    calls: &[],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[],
};

const EXAMPLE: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.example-echo",
    library: "amaterasu_echo_fixture",
    provides: &["example.echo/1"],
    calls: &[],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[RUNTIME_ID],
};

const STATE: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.state",
    library: "amaterasu_state",
    provides: &["amaterasu.state/1"],
    calls: &[],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[RUNTIME_ID],
};

const SESSION: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.session",
    library: "amaterasu_session",
    provides: &["amaterasu.session/1"],
    calls: &[],
    publishes: &["amaterasu.session.changed/1"],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[RUNTIME_ID],
};

const SESSION_HISTORY: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.session-history",
    library: "amaterasu_session_history",
    provides: &["amaterasu.session-history/1"],
    // Configuration delegates durable state calls under the original caller.
    calls: &["amaterasu.configuration/1", "amaterasu.state/1"],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[RUNTIME_ID, "amaterasu.configuration"],
};

const CONFIGURATION: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.configuration",
    library: "amaterasu_configuration",
    provides: &["amaterasu.configuration/1"],
    calls: &["amaterasu.state/1"],
    publishes: &["amaterasu.configuration.changed/1"],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[RUNTIME_ID, "amaterasu.state"],
};

const CATALOG: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.catalog",
    library: "amaterasu_catalog",
    provides: &["amaterasu.catalog/1"],
    calls: &["amaterasu.state/1"],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[RUNTIME_ID, "amaterasu.state"],
};

const APPLICATION: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.application",
    library: "amaterasu_application",
    provides: &["amaterasu.application/1"],
    calls: &[],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[RUNTIME_ID],
};

const HOST_CONFIGURATION: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.host-configuration",
    library: "amaterasu_host_configuration",
    provides: &["amaterasu.host-configuration/1"],
    calls: &["amaterasu.state/1", "amaterasu.diagnostics/1"],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[RUNTIME_ID, "amaterasu.state"],
};

const DIAGNOSTICS: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.diagnostics",
    library: "amaterasu_diagnostics",
    provides: &["amaterasu.diagnostics/1"],
    // Host configuration reads state under the original diagnostics caller.
    calls: &["amaterasu.host-configuration/1", "amaterasu.state/1"],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[RUNTIME_ID, "amaterasu.host-configuration"],
};

const STEAM: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.steam",
    library: "amaterasu_steam",
    provides: &["amaterasu.steam/1"],
    calls: &[
        "amaterasu.catalog/1",
        "amaterasu.configuration/1",
        "amaterasu.state/1",
    ],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[
        RUNTIME_ID,
        "amaterasu.catalog",
        "amaterasu.configuration",
        "amaterasu.state",
    ],
};

const PLAYNITE: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.playnite",
    library: "amaterasu_playnite",
    provides: &["amaterasu.playnite/1"],
    calls: &[
        "amaterasu.catalog/1",
        "amaterasu.configuration/1",
        "amaterasu.state/1",
    ],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[
        RUNTIME_ID,
        "amaterasu.catalog",
        "amaterasu.configuration",
        "amaterasu.state",
    ],
};

const LUTRIS: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.lutris",
    library: "amaterasu_lutris",
    provides: &["amaterasu.lutris/1"],
    calls: &[
        "amaterasu.catalog/1",
        "amaterasu.configuration/1",
        "amaterasu.state/1",
        "amaterasu.steam/1",
    ],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[
        RUNTIME_ID,
        "amaterasu.catalog",
        "amaterasu.configuration",
        "amaterasu.state",
        "amaterasu.steam",
    ],
};

const AUTHENTICATION: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.authentication",
    library: "amaterasu_authentication",
    provides: &["amaterasu.authentication/1"],
    calls: &["amaterasu.configuration/1", "amaterasu.state/1"],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[RUNTIME_ID, "amaterasu.configuration", "amaterasu.state"],
};

const PAIRING: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.pairing",
    library: "amaterasu_pairing",
    provides: &["amaterasu.pairing/1"],
    calls: &["amaterasu.state/1"],
    publishes: &[],
    native_provides: &["amaterasu.identity/1"],
    native_uses: &[],
    dependencies: &[RUNTIME_ID, "amaterasu.state"],
};

const PLATFORM_PAIRING: ComponentDefinition = ComponentDefinition {
    calls: &["amaterasu.state/1", "amaterasu.host-configuration/1"],
    dependencies: &[
        RUNTIME_ID,
        "amaterasu.state",
        "amaterasu.host-configuration",
    ],
    ..PAIRING
};

const NVHTTP: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.nvhttp",
    library: "amaterasu_nvhttp",
    provides: &["amaterasu.nvhttp/1"],
    calls: &[
        "amaterasu.configuration/1",
        "amaterasu.input/1",
        "amaterasu.pairing/1",
        "amaterasu.state/1",
        "amaterasu.catalog/1",
        "amaterasu.launch/1",
        "amaterasu.display/1",
        "amaterasu.playnite/1",
        "amaterasu.lutris/1",
        "amaterasu.application/1",
        "amaterasu.rtsp/1",
        "amaterasu.stream-session/1",
        "amaterasu.session/1",
        "amaterasu.video/1",
        "amaterasu.audio/1",
        "amaterasu.audio-encode/1",
        "amaterasu.media-transport/1",
        "amaterasu.control-transport/1",
        "amaterasu.capture-dxgi/1",
        "amaterasu.capture-wgc/1",
        "amaterasu.encode/1",
    ],
    publishes: &[],
    native_provides: &[],
    native_uses: &["amaterasu.identity/1"],
    dependencies: &[
        RUNTIME_ID,
        "amaterasu.configuration",
        "amaterasu.pairing",
        "amaterasu.state",
        "amaterasu.catalog",
        "amaterasu.launch",
        "amaterasu.display",
        "amaterasu.playnite",
        "amaterasu.rtsp",
    ],
};

const DISPLAY: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.display",
    library: "amaterasu_display",
    provides: &["amaterasu.display/1"],
    calls: &["amaterasu.state/1"],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[RUNTIME_ID, "amaterasu.state"],
};

const LAUNCH: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.launch",
    library: "amaterasu_launch",
    provides: &["amaterasu.launch/1"],
    calls: &[
        "amaterasu.catalog/1",
        "amaterasu.state/1",
        "amaterasu.session/1",
        "amaterasu.application/1",
        "amaterasu.playnite/1",
        "amaterasu.lutris/1",
    ],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[
        RUNTIME_ID,
        "amaterasu.catalog",
        "amaterasu.state",
        "amaterasu.session",
        "amaterasu.application",
        "amaterasu.playnite",
        "amaterasu.lutris",
    ],
};

const INPUT: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.input",
    library: "amaterasu_input",
    provides: &["amaterasu.input/1"],
    calls: &["amaterasu.configuration/1", "amaterasu.state/1"],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[RUNTIME_ID, "amaterasu.configuration", "amaterasu.state"],
};

const INPUT_PROBE: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.input-probe",
    library: "amaterasu_input_probe",
    provides: &["amaterasu.input/1", "test.input-probe/1"],
    calls: &[],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[RUNTIME_ID],
};

const VIRTUAL_DISPLAY: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.virtual-display",
    library: "amaterasu_virtual_display",
    provides: &["amaterasu.virtual-display/1"],
    calls: &[],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[RUNTIME_ID],
};

const CAPTURE_DXGI: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.capture-dxgi",
    library: "amaterasu_capture_dxgi",
    provides: &["amaterasu.capture-dxgi/1"],
    calls: &[],
    publishes: &[],
    native_provides: &["amaterasu.capture-dxgi/1"],
    native_uses: &[],
    dependencies: &[RUNTIME_ID],
};
const CAPTURE_WGC: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.capture-wgc",
    library: "amaterasu_capture_wgc",
    provides: &["amaterasu.capture-wgc/1"],
    calls: &[],
    publishes: &[],
    native_provides: &["amaterasu.capture-wgc/1"],
    native_uses: &[],
    dependencies: &[RUNTIME_ID],
};
const AUDIO: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.audio",
    library: "amaterasu_audio",
    provides: &["amaterasu.audio/1"],
    calls: &[],
    publishes: &[],
    native_provides: &["amaterasu.audio/1"],
    native_uses: &[],
    dependencies: &[RUNTIME_ID],
};
const AUDIO_ENCODE: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.audio-encode",
    library: "amaterasu_audio_encode",
    provides: &["amaterasu.audio-encode/1"],
    calls: &[],
    publishes: &[],
    native_provides: &["amaterasu.audio-encode/1"],
    native_uses: &[],
    dependencies: &[RUNTIME_ID],
};
const ENCODE: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.encode",
    library: "amaterasu_encode",
    provides: &["amaterasu.encode/1"],
    calls: &[],
    publishes: &[],
    native_provides: &["amaterasu.encode/1"],
    native_uses: &[],
    dependencies: &[RUNTIME_ID],
};
const VIDEO: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.video",
    library: "amaterasu_video",
    provides: &["amaterasu.video/1"],
    calls: &[
        "amaterasu.capture-dxgi/1",
        "amaterasu.capture-wgc/1",
        "amaterasu.encode/1",
    ],
    publishes: &[],
    native_provides: &["amaterasu.video/1"],
    native_uses: &[
        "amaterasu.capture-dxgi/1",
        "amaterasu.capture-wgc/1",
        "amaterasu.encode/1",
    ],
    dependencies: &[
        RUNTIME_ID,
        "amaterasu.capture-dxgi",
        "amaterasu.capture-wgc",
        "amaterasu.encode",
    ],
};
const MEDIA_TRANSPORT: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.media-transport",
    library: "amaterasu_media_transport",
    provides: &["amaterasu.media-transport/1"],
    calls: &[],
    publishes: &[],
    native_provides: &["amaterasu.media-transport/1"],
    native_uses: &[],
    dependencies: &[RUNTIME_ID],
};
const CONTROL_TRANSPORT: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.control-transport",
    library: "amaterasu_control_transport",
    provides: &["amaterasu.control-transport/1"],
    calls: &[],
    publishes: &[],
    native_provides: &["amaterasu.control-transport/1"],
    native_uses: &[],
    dependencies: &[RUNTIME_ID],
};
const FRAME_LIMITER: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.frame-limiter",
    library: "amaterasu_frame_limiter",
    provides: &["amaterasu.frame-limiter/1"],
    calls: &[],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[RUNTIME_ID],
};
const STREAM_SESSION: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.stream-session",
    library: "amaterasu_stream_session",
    provides: &["amaterasu.stream-session/1"],
    calls: &[
        "amaterasu.input/1",
        "amaterasu.session/1",
        "amaterasu.display/1",
        "amaterasu.state/1",
        "amaterasu.video/1",
        "amaterasu.audio/1",
        "amaterasu.audio-encode/1",
        "amaterasu.media-transport/1",
        "amaterasu.control-transport/1",
        "amaterasu.capture-dxgi/1",
        "amaterasu.capture-wgc/1",
        "amaterasu.encode/1",
        "amaterasu.session-history/1",
        "amaterasu.frame-limiter/1",
    ],
    publishes: &[],
    native_provides: &[],
    native_uses: &[
        "amaterasu.video/1",
        "amaterasu.audio/1",
        "amaterasu.audio-encode/1",
        "amaterasu.media-transport/1",
        "amaterasu.control-transport/1",
    ],
    dependencies: &[
        "amaterasu.input",
        RUNTIME_ID,
        "amaterasu.session",
        "amaterasu.display",
        "amaterasu.state",
        "amaterasu.session-history",
        "amaterasu.video",
        "amaterasu.audio",
        "amaterasu.audio-encode",
        "amaterasu.media-transport",
        "amaterasu.control-transport",
        "amaterasu.frame-limiter",
    ],
};
const STREAM_SOURCE: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.stream-source",
    library: "amaterasu_stream_source",
    provides: &[
        "amaterasu.video/1",
        "amaterasu.audio/1",
        "test.stream-source/1",
    ],
    calls: &[],
    publishes: &[],
    native_provides: &["amaterasu.video/1", "amaterasu.audio/1"],
    native_uses: &[],
    dependencies: &[RUNTIME_ID],
};
const RTSP: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.rtsp",
    library: "amaterasu_rtsp",
    provides: &["amaterasu.rtsp/1"],
    calls: &[
        "amaterasu.input/1",
        "amaterasu.stream-session/1",
        "amaterasu.session/1",
        "amaterasu.display/1",
        "amaterasu.state/1",
        "amaterasu.launch/1",
        "amaterasu.video/1",
        "amaterasu.audio/1",
        "amaterasu.audio-encode/1",
        "amaterasu.media-transport/1",
        "amaterasu.control-transport/1",
        "amaterasu.capture-dxgi/1",
        "amaterasu.capture-wgc/1",
        "amaterasu.encode/1",
    ],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[
        RUNTIME_ID,
        "amaterasu.stream-session",
        "amaterasu.display",
        "amaterasu.state",
        "amaterasu.launch",
    ],
};

// The desktop smoke profile follows a manually-added Moonlight desktop path.
// It deliberately excludes app-launch integrations and WGC while retaining
// catalog reads so NVHTTP can fail closed if the reserved desktop ID collides.
const NVHTTP_DESKTOP_SMOKE: ComponentDefinition = ComponentDefinition {
    calls: &[
        "amaterasu.configuration/1",
        "amaterasu.input/1",
        "amaterasu.pairing/1",
        "amaterasu.state/1",
        "amaterasu.catalog/1",
        "amaterasu.display/1",
        "amaterasu.rtsp/1",
        "amaterasu.stream-session/1",
        "amaterasu.session/1",
        "amaterasu.video/1",
        "amaterasu.audio/1",
        "amaterasu.audio-encode/1",
        "amaterasu.media-transport/1",
        "amaterasu.control-transport/1",
        "amaterasu.capture-dxgi/1",
        "amaterasu.encode/1",
    ],
    dependencies: &[
        RUNTIME_ID,
        "amaterasu.configuration",
        "amaterasu.input",
        "amaterasu.pairing",
        "amaterasu.state",
        "amaterasu.catalog",
        "amaterasu.display",
        "amaterasu.rtsp",
        "amaterasu.stream-session",
        "amaterasu.session",
        "amaterasu.video",
        "amaterasu.audio",
        "amaterasu.audio-encode",
        "amaterasu.media-transport",
        "amaterasu.control-transport",
        "amaterasu.capture-dxgi",
        "amaterasu.encode",
    ],
    ..NVHTTP
};

const VIDEO_DXGI_SMOKE: ComponentDefinition = ComponentDefinition {
    calls: &["amaterasu.capture-dxgi/1", "amaterasu.encode/1"],
    native_uses: &["amaterasu.capture-dxgi/1", "amaterasu.encode/1"],
    dependencies: &[RUNTIME_ID, "amaterasu.capture-dxgi", "amaterasu.encode"],
    ..VIDEO
};

const STREAM_SESSION_DXGI_SMOKE: ComponentDefinition = ComponentDefinition {
    calls: &[
        "amaterasu.input/1",
        "amaterasu.session/1",
        "amaterasu.display/1",
        "amaterasu.state/1",
        "amaterasu.video/1",
        "amaterasu.audio/1",
        "amaterasu.audio-encode/1",
        "amaterasu.media-transport/1",
        "amaterasu.control-transport/1",
        "amaterasu.capture-dxgi/1",
        "amaterasu.encode/1",
        "amaterasu.session-history/1",
        "amaterasu.frame-limiter/1",
    ],
    dependencies: &[
        "amaterasu.input",
        RUNTIME_ID,
        "amaterasu.session",
        "amaterasu.display",
        "amaterasu.state",
        "amaterasu.session-history",
        "amaterasu.video",
        "amaterasu.audio",
        "amaterasu.audio-encode",
        "amaterasu.media-transport",
        "amaterasu.control-transport",
        "amaterasu.capture-dxgi",
        "amaterasu.encode",
        "amaterasu.frame-limiter",
    ],
    ..STREAM_SESSION
};

const RTSP_DESKTOP_SMOKE: ComponentDefinition = ComponentDefinition {
    calls: &[
        "amaterasu.input/1",
        "amaterasu.stream-session/1",
        "amaterasu.session/1",
        "amaterasu.display/1",
        "amaterasu.state/1",
        "amaterasu.video/1",
        "amaterasu.audio/1",
        "amaterasu.audio-encode/1",
        "amaterasu.media-transport/1",
        "amaterasu.control-transport/1",
        "amaterasu.capture-dxgi/1",
        "amaterasu.encode/1",
    ],
    dependencies: &[
        RUNTIME_ID,
        "amaterasu.input",
        "amaterasu.stream-session",
        "amaterasu.session",
        "amaterasu.display",
        "amaterasu.state",
        "amaterasu.video",
        "amaterasu.audio",
        "amaterasu.audio-encode",
        "amaterasu.media-transport",
        "amaterasu.control-transport",
        "amaterasu.capture-dxgi",
        "amaterasu.encode",
    ],
    ..RTSP
};
const STREAM_SESSION_PROBE: ComponentDefinition = ComponentDefinition {
    dependencies: &[
        "amaterasu.input-probe",
        RUNTIME_ID,
        "amaterasu.session",
        "amaterasu.display",
        "amaterasu.state",
        "amaterasu.session-history",
        "amaterasu.stream-source",
        "amaterasu.audio-encode",
        "amaterasu.media-transport",
        "amaterasu.control-transport",
        "amaterasu.frame-limiter",
    ],
    ..STREAM_SESSION
};

const MEDIA_PROBE: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.media-probe",
    library: "amaterasu_media_probe",
    provides: &["test.media-probe/1"],
    calls: &[
        "amaterasu.capture-dxgi/1",
        "amaterasu.capture-wgc/1",
        "amaterasu.audio-encode/1",
        "amaterasu.encode/1",
        "amaterasu.video/1",
        "amaterasu.media-transport/1",
        "amaterasu.control-transport/1",
    ],
    publishes: &[],
    native_provides: &[],
    native_uses: &[
        "amaterasu.capture-dxgi/1",
        "amaterasu.capture-wgc/1",
        "amaterasu.audio/1",
        "amaterasu.audio-encode/1",
        "amaterasu.encode/1",
        "amaterasu.video/1",
        "amaterasu.media-transport/1",
        "amaterasu.control-transport/1",
    ],
    dependencies: &[
        RUNTIME_ID,
        "amaterasu.capture-dxgi",
        "amaterasu.capture-wgc",
        "amaterasu.audio",
        "amaterasu.audio-encode",
        "amaterasu.encode",
        "amaterasu.video",
        "amaterasu.media-transport",
        "amaterasu.control-transport",
    ],
};

const MANAGEMENT: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.management",
    library: "amaterasu_management",
    provides: &["amaterasu.management/1"],
    calls: &[
        // Loopback `/healthz` asks this component's fixed, status-only
        // operation to drive bounded checks of its imported dependencies.
        "amaterasu.management/1",
        "amaterasu.authentication/1",
        "amaterasu.state/1",
        "amaterasu.catalog/1",
        "amaterasu.configuration/1",
        "amaterasu.session/1",
        "amaterasu.application/1",
        "amaterasu.input/1",
        "amaterasu.steam/1",
        "amaterasu.playnite/1",
        "amaterasu.lutris/1",
        "amaterasu.diagnostics/1",
        "amaterasu.host-configuration/1",
        "amaterasu.pairing/1",
        "amaterasu.display/1",
        "amaterasu.launch/1",
        "amaterasu.capture-dxgi/1",
        "amaterasu.capture-wgc/1",
        "amaterasu.audio/1",
        "amaterasu.audio-encode/1",
        "amaterasu.encode/1",
        "amaterasu.video/1",
        "amaterasu.nvhttp/1",
        // NVHTTP applies base-port changes through RTSP, which preflights
        // its media and control UDP listeners under the same admin request.
        "amaterasu.rtsp/1",
        "amaterasu.media-transport/1",
        "amaterasu.control-transport/1",
        "amaterasu.session-history/1",
    ],
    publishes: &[],
    native_provides: &[],
    native_uses: &[],
    dependencies: &[
        RUNTIME_ID,
        "amaterasu.authentication",
        "amaterasu.state",
        "amaterasu.catalog",
        "amaterasu.configuration",
        "amaterasu.session",
        "amaterasu.application",
        "amaterasu.input",
        "amaterasu.steam",
        "amaterasu.playnite",
        "amaterasu.lutris",
        "amaterasu.diagnostics",
        "amaterasu.host-configuration",
        "amaterasu.pairing",
        "amaterasu.display",
        "amaterasu.launch",
        "amaterasu.capture-dxgi",
        "amaterasu.capture-wgc",
        "amaterasu.audio",
        "amaterasu.audio-encode",
        "amaterasu.encode",
        "amaterasu.video",
        "amaterasu.nvhttp",
        "amaterasu.session-history",
    ],
};

impl PackageProfile {
    pub fn components(self) -> &'static [ComponentDefinition] {
        match self {
            Self::Example => &[RUNTIME, EXAMPLE],
            Self::MediaProbe => &[
                RUNTIME,
                CAPTURE_DXGI,
                CAPTURE_WGC,
                AUDIO,
                AUDIO_ENCODE,
                ENCODE,
                VIDEO,
                MEDIA_TRANSPORT,
                CONTROL_TRANSPORT,
                MEDIA_PROBE,
            ],
            Self::StreamProbe => &[
                RUNTIME,
                STATE,
                CONFIGURATION,
                CATALOG,
                APPLICATION,
                STEAM,
                PLAYNITE,
                LUTRIS,
                LAUNCH,
                DISPLAY,
                PAIRING,
                NVHTTP,
                SESSION,
                SESSION_HISTORY,
                STREAM_SOURCE,
                INPUT_PROBE,
                AUDIO_ENCODE,
                MEDIA_TRANSPORT,
                CONTROL_TRANSPORT,
                FRAME_LIMITER,
                STREAM_SESSION_PROBE,
                RTSP,
            ],
            Self::DesktopStreamSmoke => &[
                RUNTIME,
                STATE,
                SESSION,
                SESSION_HISTORY,
                CONFIGURATION,
                CATALOG,
                HOST_CONFIGURATION,
                DIAGNOSTICS,
                PLATFORM_PAIRING,
                NVHTTP_DESKTOP_SMOKE,
                DISPLAY,
                INPUT,
                CAPTURE_DXGI,
                AUDIO,
                AUDIO_ENCODE,
                ENCODE,
                VIDEO_DXGI_SMOKE,
                MEDIA_TRANSPORT,
                CONTROL_TRANSPORT,
                FRAME_LIMITER,
                STREAM_SESSION_DXGI_SMOKE,
                RTSP_DESKTOP_SMOKE,
            ],
            Self::Platform => &[
                RUNTIME,
                STATE,
                SESSION,
                SESSION_HISTORY,
                CONFIGURATION,
                CATALOG,
                APPLICATION,
                HOST_CONFIGURATION,
                DIAGNOSTICS,
                STEAM,
                PLAYNITE,
                LUTRIS,
                AUTHENTICATION,
                MANAGEMENT,
                PLATFORM_PAIRING,
                NVHTTP,
                DISPLAY,
                LAUNCH,
                INPUT,
                VIRTUAL_DISPLAY,
                CAPTURE_DXGI,
                CAPTURE_WGC,
                AUDIO,
                AUDIO_ENCODE,
                ENCODE,
                VIDEO,
                MEDIA_TRANSPORT,
                CONTROL_TRANSPORT,
                FRAME_LIMITER,
                STREAM_SESSION,
                RTSP,
            ],
        }
    }
}

impl ComponentDefinition {
    pub fn grants(&self) -> Grants {
        Grants {
            provide: names(self.provides),
            native_provide: names(self.native_provides),
            native_use: names(self.native_uses),
            call: names(self.calls),
            publish: names(self.publishes),
            ..Default::default()
        }
    }
}

// Authorization generations are profile-specific and never reused. Advance a profile's
// generation whenever its administrator_principals or allowed_scopes change, including a
// revert; TTL-only edits do not bump it.
pub fn development_policy(root: &Path) -> PackageResult<RuntimeConfig> {
    let mut policy = RuntimeConfig::default();
    let mut console = Grants::default();

    for component in PackageProfile::Example
        .components()
        .iter()
        .chain(PackageProfile::Platform.components())
        .chain(PackageProfile::MediaProbe.components())
        .chain(PackageProfile::StreamProbe.components())
    {
        if component.id == RUNTIME_ID {
            continue;
        }
        policy
            .principals
            .insert(component.id.into(), component.grants());
        console.call.extend(names(component.provides));
        console.subscribe.extend(names(component.publishes));
    }
    policy
        .principals
        .insert("amaterasu.console".into(), console);

    let state_directory = root.join("data").join("state");
    create_private_directory(&state_directory)?;
    let state_directory = state_directory
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let history_directory = state_directory.join("session-history");
    create_private_directory(&history_directory)?;
    let history_database = history_directory
        .canonicalize()
        .map_err(|error| error.to_string())?
        .join("history.db");
    policy.settings = BTreeMap::from([
        (
            "amaterasu.rtsp".into(),
            json!({"enabled":false,"allow_remote":false,"address_family":"ipv4","bind_address":"","allowed_principals":[]}),
        ),
        (
            "amaterasu.stream-session".into(),
            json!({"enabled":false,"allow_remote":false,"allowed_principals":[]}),
        ),
        ("amaterasu.video".into(), json!({"enabled": false})),
        (
            "amaterasu.control-transport".into(),
            json!({"enabled": false, "allow_remote": false, "allowed_principals": []}),
        ),
        ("amaterasu.capture-dxgi".into(), json!({"enabled": false})),
        ("amaterasu.encode".into(), json!({"enabled": false})),
        ("amaterasu.capture-wgc".into(), json!({"enabled": false})),
        ("amaterasu.audio".into(), json!({"enabled": false})),
        ("amaterasu.audio-encode".into(), json!({"enabled": false})),
        (
            "amaterasu.media-transport".into(),
            json!({"enabled": false, "allow_remote": false, "allowed_principals": []}),
        ),
        (
            "amaterasu.virtual-display".into(),
            json!({"enabled": false}),
        ),
        (
            "amaterasu.catalog".into(),
            json!({
                "launch_principals": ["amaterasu.launch"],
                "export_principals": {"amaterasu.nvhttp": "amaterasu.management"},
                "steam_id_export_principals": {"amaterasu.lutris": "amaterasu.management"},
                "import_principals": {
                    "amaterasu.steam": "amaterasu.management",
                    "amaterasu.playnite": "amaterasu.management",
                    "amaterasu.lutris": "amaterasu.management",
                },
            }),
        ),
        ("amaterasu.input".into(), json!({"allow_injection": false})),
        ("amaterasu.input-probe".into(), json!({"enabled": false})),
        (
            "amaterasu.pairing".into(),
            json!({
                "administrator_principals": ["amaterasu.console", "amaterasu.management"],
                "transport_principals": ["amaterasu.nvhttp"],
                "host_configuration": true,
            }),
        ),
        ("amaterasu.display".into(), json!({"allow_mutation": false})),
        (
            "amaterasu.nvhttp".into(),
            json!({"enabled": false, "address_family":"ipv4", "bind_address":""}),
        ),
        (
            "amaterasu.state".into(),
            json!({"data_directory": state_directory}),
        ),
        (
            "amaterasu.session-history".into(),
            json!({"database_path": history_database.to_string_lossy()}),
        ),
        (
            "amaterasu.authentication".into(),
            json!({
                "administrator_principals": ["amaterasu.console", "amaterasu.management"],
                "authorization_policy_generation": 2,
                "session_token_ttl_seconds": 7200,
                "remember_me_refresh_token_ttl_seconds": 604800,
                "allowed_scopes": [
                    "catalog.read", "catalog.write", "configuration.read", "configuration.write",
                    "session.read", "session.write", "application.read", "application.launch",
                    "application.terminate", "steam.read", "steam.refresh", "lutris.read", "diagnostics.read", "diagnostics.write", "pairing.admin", "display.read", "display.write", "media.read"
                ]
            }),
        ),
    ]);
    policy
        .principals
        .insert("amaterasu.pairing".into(), PLATFORM_PAIRING.grants());
    Ok(policy)
}

/// Development-only policy for a manually-added Moonlight desktop smoke host.
/// The default binds every stream listener to loopback. `allow_remote` opts in
/// to wildcard listeners in the host policy; it does not change firewall or
/// machine networking settings. Before streaming, replace the output placeholder
/// in bootstrap settings with an ID from the DXGI provider's enumerate action;
/// output IDs are topology-specific and capture does not guess a display.
pub fn desktop_stream_smoke_policy(
    root: &Path,
    allow_remote: bool,
) -> PackageResult<RuntimeConfig> {
    let mut policy = development_policy(root)?;
    let components = PackageProfile::DesktopStreamSmoke.components();
    let component_ids: BTreeSet<_> = components.iter().map(|component| component.id).collect();
    policy
        .principals
        .retain(|principal, _| principal == "amaterasu.console" || component_ids.contains(principal.as_str()));
    let mut console = Grants::default();
    for component in components {
        if component.id == RUNTIME_ID {
            continue;
        }
        policy
            .principals
            .insert(component.id.into(), component.grants());
        console.call.extend(names(component.provides));
        console.subscribe.extend(names(component.publishes));
    }
    policy
        .principals
        .insert("amaterasu.console".into(), console);

    let remote_stream = allow_remote;
    policy.settings.retain(|principal, _| component_ids.contains(principal.as_str()));
    policy.settings.extend(BTreeMap::from([
        (
            "amaterasu.rtsp".into(),
            json!({"enabled":true,"allow_remote":remote_stream,"address_family":"ipv4","bind_address":"","allowed_principals":["amaterasu.nvhttp"]}),
        ),
        (
            "amaterasu.stream-session".into(),
            json!({"enabled":true,"allow_remote":remote_stream,"allowed_principals":["amaterasu.nvhttp"],"enable_input":true}),
        ),
        ("amaterasu.video".into(), json!({"enabled":true,"capture_providers":["dxgi"]})),
        (
            "amaterasu.control-transport".into(),
            json!({"enabled":true,"allow_remote":remote_stream,"allowed_principals":["amaterasu.stream-session"]}),
        ),
        ("amaterasu.capture-dxgi".into(), json!({"enabled":true})),
        ("amaterasu.encode".into(), json!({"enabled":true})),
        ("amaterasu.audio".into(), json!({"enabled":true})),
        ("amaterasu.audio-encode".into(), json!({"enabled":true})),
        (
            "amaterasu.media-transport".into(),
            json!({"enabled":true,"allow_remote":remote_stream,"allowed_principals":["amaterasu.stream-session"]}),
        ),
        (
            "amaterasu.catalog".into(),
            json!({"export_principals":{"amaterasu.nvhttp":"amaterasu.console"}}),
        ),
        (
            "amaterasu.input".into(),
            json!({"allow_injection":true,"windows_backend":"vhf_companion"}),
        ),
        (
            "amaterasu.pairing".into(),
            json!({"administrator_principals":["amaterasu.console"],"transport_principals":["amaterasu.nvhttp"],"host_configuration":true}),
        ),
        ("amaterasu.display".into(), json!({"allow_mutation":false})),
        (
            "amaterasu.nvhttp".into(),
            json!({
                "enabled":true,
                "address_family":"ipv4",
                "bind_address":"",
                "allow_remote":remote_stream,
                "streaming":{
                    "desktop":{"enabled":true},
                    "capture":{"capture_provider":"dxgi","output":"SELECT_OUTPUT_ID","encoder":0,"audio_endpoint":"default"},
                    "profile":{"max_width":1920,"max_height":1080,"max_fps":60,"bitrate_kbps":10000,"max_reference_frames":1,"packet_size":1024,"fec_percentage":20,"min_parity_shards":2,"max_fec_blocks":1,"packet_ms":10,"audio_bitrate":128000,"encrypt_video":true,"encrypt_audio":true,"permit_legacy_audio_iv_wrap":false}
                }
            }),
        ),
    ]));
    Ok(policy)
}

/// Policy embedded in the signed installer seed.
///
/// This is deliberately separate from [`development_policy`]: an installed
/// host must not inherit the development console, generated development key,
/// or its all-disabled media defaults.  The initial production surface is
/// intentionally narrow.  It exposes management and Moonlight-compatible
/// control only on loopback, while leaving hardware-dependent media disabled
/// until a signed, machine-qualified media policy is available.
pub fn installer_policy(
    web_directory: &str,
    trusted_keys: BTreeMap<String, String>,
) -> RuntimeConfig {
    let mut policy = RuntimeConfig::default();
    for component in PackageProfile::Platform.components() {
        if component.id != RUNTIME_ID {
            policy
                .principals
                .insert(component.id.into(), component.grants());
        }
    }

    policy.settings = BTreeMap::from([
        (
            "amaterasu.management".into(),
            json!({
                "enabled": true,
                "bind": "127.0.0.1:47990",
                "allowed_origins": [],
                "web": {"directory": web_directory, "trusted_keys": trusted_keys},
            }),
        ),
        (
            "amaterasu.nvhttp".into(),
            json!({
                "enabled": true,
                "http_bind": "127.0.0.1:47989",
                "https_bind": "127.0.0.1:47984",
                "address_family": "ipv4",
                "bind_address": "",
                "allow_remote": false,
            }),
        ),
        (
            "amaterasu.catalog".into(),
            json!({
                "launch_principals": ["amaterasu.launch"],
                "export_principals": {"amaterasu.nvhttp": "amaterasu.management"},
                "steam_id_export_principals": {"amaterasu.lutris": "amaterasu.management"},
                "import_principals": {
                    "amaterasu.steam": "amaterasu.management",
                    "amaterasu.playnite": "amaterasu.management",
                    "amaterasu.lutris": "amaterasu.management",
                },
            }),
        ),
        (
            "amaterasu.authentication".into(),
            json!({
                "administrator_principals": ["amaterasu.management"],
                "authorization_policy_generation": 3,
                "session_token_ttl_seconds": 7200,
                "remember_me_refresh_token_ttl_seconds": 604800,
                "allowed_scopes": [
                    "catalog.read", "catalog.write", "configuration.read", "configuration.write",
                    "session.read", "session.write", "application.read", "application.launch",
                    "application.terminate", "steam.read", "steam.refresh", "lutris.read", "diagnostics.read",
                    "diagnostics.write", "pairing.admin", "display.read", "display.write", "media.read"
                ],
            }),
        ),
        (
            "amaterasu.pairing".into(),
            json!({
                "administrator_principals": ["amaterasu.management"],
                "transport_principals": ["amaterasu.nvhttp"],
                "host_configuration": true,
            }),
        ),
        ("amaterasu.input".into(), json!({"allow_injection": false})),
        ("amaterasu.display".into(), json!({"allow_mutation": false})),
        (
            "amaterasu.virtual-display".into(),
            json!({"enabled": false}),
        ),
        ("amaterasu.capture-dxgi".into(), json!({"enabled": false})),
        ("amaterasu.capture-wgc".into(), json!({"enabled": false})),
        ("amaterasu.audio".into(), json!({"enabled": false})),
        ("amaterasu.audio-encode".into(), json!({"enabled": false})),
        ("amaterasu.encode".into(), json!({"enabled": false})),
        ("amaterasu.video".into(), json!({"enabled": false})),
        (
            "amaterasu.media-transport".into(),
            json!({"enabled": false, "allow_remote": false, "allowed_principals": []}),
        ),
        (
            "amaterasu.control-transport".into(),
            json!({"enabled": false, "allow_remote": false, "allowed_principals": []}),
        ),
        (
            "amaterasu.stream-session".into(),
            json!({"enabled": false, "allow_remote": false, "allowed_principals": []}),
        ),
        (
            "amaterasu.rtsp".into(),
            json!({"enabled": false, "allow_remote": false, "address_family":"ipv4", "bind_address":"", "allowed_principals": []}),
        ),
    ]);
    policy
}

fn names(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|name| (*name).to_owned()).collect()
}

#[cfg(test)]
mod tests;
