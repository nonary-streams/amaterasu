//! Trusted product assembly for replaceable client-session presentation.
//!
//! The protocol consumer selects the public service explicitly. Product-specific
//! controls and retained ownership stay in the selected native plugin.
use super::{ComponentDefinition, RUNTIME_ID, X11_VIDEO, platform_extensions};
use amaterasu_contracts::{ApiMethodApproval, RuntimeConfig};
use amaterasu_module_sdk::session_presentation;
use serde_json::json;

const PROVIDER_DEPENDENCIES: &[&str] = &[
    RUNTIME_ID,
    "amaterasu.configuration",
    "amaterasu.state",
    "amaterasu.catalog",
    "amaterasu.virtual-display",
    "amaterasu.display",
    "amaterasu.rtsp",
    "amaterasu.launch",
    "amaterasu.pairing",
];

pub(super) const COMPONENT: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.remote-session",
    library: "amaterasu_remote_session",
    provides: &["nonary.session-presentation/1", "nonary.remote-session/1"],
    calls: &[
        "amaterasu.configuration/1",
        "amaterasu.state/1",
        "amaterasu.catalog/1",
        "amaterasu.virtual-display/1",
        "amaterasu.display/1",
        "amaterasu.rtsp/1",
        "amaterasu.launch/1",
        "amaterasu.pairing/1",
        "amaterasu.video/1",
    ],
    publishes: &[],
    dependencies: &append::<{ PROVIDER_DEPENDENCIES.len() + 1 }>(
        PROVIDER_DEPENDENCIES,
        &["amaterasu.video"],
    ),
    native_provides: &[],
    native_uses: &[],
};

pub(super) const LINUX_COMPONENT: ComponentDefinition = ComponentDefinition {
    dependencies: &append::<{ PROVIDER_DEPENDENCIES.len() + 1 }>(
        PROVIDER_DEPENDENCIES,
        &[super::linux_session::VIDEO_COMPONENT_ID],
    ),
    ..COMPONENT
};

pub(super) const BLACK_CAPTURE: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.capture-black",
    library: "amaterasu_capture_black",
    provides: &["amaterasu.capture-black/1"],
    calls: &[],
    publishes: &[],
    dependencies: &[RUNTIME_ID],
    native_provides: &["amaterasu.capture-black/1"],
    native_uses: &[],
};

pub(super) const RETAINED_DISPLAY: ComponentDefinition = ComponentDefinition {
    calls: &append::<{ super::DISPLAY.calls.len() + 1 }>(
        super::DISPLAY.calls,
        &["amaterasu.virtual-display/1"],
    ),
    dependencies: &append::<{ super::DISPLAY.dependencies.len() + 1 }>(
        super::DISPLAY.dependencies,
        &["amaterasu.virtual-display"],
    ),
    ..super::DISPLAY
};

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
    let mut extra = 0;
    while extra < right.len() {
        result[index] = right[extra];
        index += 1;
        extra += 1;
    }
    result
}

pub(super) const PLATFORM_TRANSPORT: ComponentDefinition = ComponentDefinition {
    calls: &append::<{ platform_extensions::TRANSPORT.calls.len() + 2 }>(
        platform_extensions::TRANSPORT.calls,
        &["nonary.session-presentation/1", "amaterasu.capture-black/1"],
    ),
    dependencies: &append::<{ platform_extensions::TRANSPORT.dependencies.len() + 1 }>(
        platform_extensions::TRANSPORT.dependencies,
        &[COMPONENT.id],
    ),
    ..platform_extensions::TRANSPORT
};

pub(super) const PLATFORM_STREAM_CONTROL: ComponentDefinition = ComponentDefinition {
    calls: &append::<{ platform_extensions::STREAM_CONTROL.calls.len() + 1 }>(
        platform_extensions::STREAM_CONTROL.calls,
        &["amaterasu.capture-black/1"],
    ),
    ..platform_extensions::STREAM_CONTROL
};

pub(super) const PLATFORM_STREAM: ComponentDefinition = ComponentDefinition {
    calls: &append::<{ platform_extensions::STREAM.calls.len() + 1 }>(
        platform_extensions::STREAM.calls,
        &["amaterasu.capture-black/1"],
    ),
    ..platform_extensions::STREAM
};

pub(super) const PLATFORM_VIDEO: ComponentDefinition = ComponentDefinition {
    calls: &append::<{ X11_VIDEO.calls.len() + 1 }>(
        X11_VIDEO.calls,
        &["amaterasu.capture-black/1"],
    ),
    native_uses: &append::<{ X11_VIDEO.native_uses.len() + 1 }>(
        X11_VIDEO.native_uses,
        &["amaterasu.capture-black/1"],
    ),
    dependencies: &append::<{ X11_VIDEO.dependencies.len() + 1 }>(
        X11_VIDEO.dependencies,
        &[BLACK_CAPTURE.id],
    ),
    ..X11_VIDEO
};

pub(super) const PLATFORM_MANAGEMENT: ComponentDefinition = ComponentDefinition {
    calls: &append::<{ platform_extensions::ADMIN.calls.len() + 1 }>(
        platform_extensions::ADMIN.calls,
        &["nonary.remote-session/1"],
    ),
    dependencies: &append::<{ platform_extensions::ADMIN.dependencies.len() + 1 }>(
        platform_extensions::ADMIN.dependencies,
        &[COMPONENT.id],
    ),
    ..platform_extensions::ADMIN
};

pub(super) const EXAMPLE_MANAGEMENT: ComponentDefinition = ComponentDefinition {
    calls: &append::<{ platform_extensions::EXAMPLE_ADMIN.calls.len() + 1 }>(
        platform_extensions::EXAMPLE_ADMIN.calls,
        &["nonary.remote-session/1"],
    ),
    dependencies: &append::<{ platform_extensions::EXAMPLE_ADMIN.dependencies.len() + 1 }>(
        platform_extensions::EXAMPLE_ADMIN.dependencies,
        &[COMPONENT.id],
    ),
    ..platform_extensions::EXAMPLE_ADMIN
};

/// Complete the selected product graph without granting new hardware authority.
/// Disabled capture, input, display mutation and remote listeners retain their
/// existing values. The black source only supplies bounded in-memory pixels.
pub(super) fn install(policy: &mut RuntimeConfig) -> Result<(), String> {
    if !policy.principals.contains_key(COMPONENT.id) {
        return Ok(());
    }
    if !policy.principals[COMPONENT.id]
        .provide
        .contains(session_presentation::SERVICE)
        || !policy
            .principals
            .get("amaterasu.nvhttp")
            .is_some_and(|grants| grants.call.contains(session_presentation::SERVICE))
    {
        return Err("selected session presentation service lacks explicit grants".into());
    }
    let mut candidate = policy.clone();
    configure_native_settings(&mut candidate)?;
    configure_application_retirement_observer(&mut candidate)?;
    configure_catalog_profile_owner(&mut candidate)?;
    configure_capture_routes(&mut candidate);
    approve_protocol_methods(&mut candidate)?;
    *policy = candidate;
    Ok(())
}

fn configure_native_settings(policy: &mut RuntimeConfig) -> Result<(), String> {
    policy
        .settings
        .entry(COMPONENT.id.into())
        .or_insert_with(|| {
            json!({
                "enabled": true,
                "allowed_principals": ["amaterasu.nvhttp"],
                "source_verifiers": ["amaterasu.nvhttp"],
                "remote_monitor_mute_audio": false,
                "remote_monitor_disconnect_on_stream_end": false,
                "remote_monitor_disconnect_on_client_disconnect": false,
                "remote_monitor_terminate_on_first_request": false,
                "remote_monitor_confirm_app_replacement": true,
                "show_remote_input": true,
                "show_remote_monitor": true,
                "max_clients": 16,
                "remote_monitor_layout": {"version":1,"placements":{}},
            })
        });
    policy
        .settings
        .entry(BLACK_CAPTURE.id.into())
        .or_insert_with(|| json!({"enabled":true}));
    if let Some(settings) = policy.settings.get_mut("amaterasu.virtual-display") {
        settings["target_verifiers"] = json!(["amaterasu.display"]);
    }
    if let Some(settings) = policy.settings.get_mut("amaterasu.pairing") {
        settings["authority_observer_principals"] = json!([COMPONENT.id]);
    }
    if let Some(settings) = policy.settings.get_mut("amaterasu.rtsp") {
        configure_transport_retirement_observer(settings)?;
        settings["max_live_tickets"] = json!(32);
    }
    if let Some(settings) = policy.settings.get_mut("amaterasu.stream-session") {
        settings["max_live_sessions"] = json!(32);
    }
    if let Some(settings) = policy.settings.get_mut("amaterasu.session-lifecycle") {
        if settings["capture_binding_mode"] != "external" {
            return Err(
                "borrowed remote capture requires the external lifecycle binding contract".into(),
            );
        }
        settings["borrowed_capture_callers"] = json!(["amaterasu.nvhttp", "amaterasu.rtsp"]);
    }
    Ok(())
}

fn configure_transport_retirement_observer(settings: &mut serde_json::Value) -> Result<(), String> {
    let approved_owner = settings["allowed_principals"]
        .as_array()
        .is_some_and(|owners| owners.contains(&json!("amaterasu.nvhttp")));
    if !approved_owner {
        // An inert seed does not select an RTSP owner. It must not gain one
        // merely to make a remote observer's recovery request admissible.
        return Ok(());
    }
    let mut observers = settings["retirement_status_principals"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !observers.contains(&json!(COMPONENT.id)) {
        observers.push(json!(COMPONENT.id));
    }
    if settings.get("retirement_status_owners").is_none() {
        settings["retirement_status_owners"] = json!({});
    }
    if settings["retirement_status_owners"]
        .get(COMPONENT.id)
        .is_some_and(|owner| owner != "amaterasu.nvhttp")
    {
        return Err("transport retirement observer conflicts with selected stream owner".into());
    }
    settings["retirement_status_principals"] = json!(observers);
    settings["retirement_status_owners"][COMPONENT.id] = json!("amaterasu.nvhttp");
    Ok(())
}

fn configure_application_retirement_observer(policy: &mut RuntimeConfig) -> Result<(), String> {
    let launch = policy
        .settings
        .entry("amaterasu.launch".into())
        .or_insert_with(|| json!({}));
    if launch.get("application_retirement_observers").is_none() {
        launch["application_retirement_observers"] = json!({});
    }
    if launch["application_retirement_observers"]
        .get(COMPONENT.id)
        .is_some_and(|owner| owner != "amaterasu.nvhttp")
    {
        return Err(
            "application retirement observer conflicts with selected transport owner".into(),
        );
    }
    launch["application_retirement_observers"][COMPONENT.id] = json!("amaterasu.nvhttp");
    Ok(())
}

fn configure_catalog_profile_owner(policy: &mut RuntimeConfig) -> Result<(), String> {
    let catalog = policy
        .settings
        .get_mut("amaterasu.catalog")
        .ok_or("remote monitor profiles require the selected catalog")?;
    let owner = catalog["export_principals"]["amaterasu.nvhttp"]
        .as_str()
        .ok_or("remote monitor profile owner must match the authenticated protocol catalog")?
        .to_owned();
    if catalog.get("profile_principals").is_none() {
        catalog["profile_principals"] = json!({});
    }
    let selected = &catalog["profile_principals"][COMPONENT.id];
    if !selected.is_null() && selected != &json!(owner) {
        return Err("remote monitor profile owner conflicts with catalog policy".into());
    }
    catalog["profile_principals"][COMPONENT.id] = json!(owner);
    Ok(())
}

fn configure_capture_routes(policy: &mut RuntimeConfig) {
    let exact_capture = if policy
        .settings
        .get("amaterasu.linux-session-video")
        .is_some_and(|settings| {
            settings["external_capture_providers"]
                .get("virtual_screen")
                .is_some()
        }) {
        "virtual_screen"
    } else if policy
        .settings
        .contains_key("amaterasu.linux-session-video")
    {
        "vkms"
    } else {
        "dxgi"
    };
    let mut exact_routes = json!({"managed_output":exact_capture});
    let linux_video = policy
        .settings
        .contains_key("amaterasu.linux-session-video");
    if !linux_video {
        exact_routes["desktop_output"] = json!("dxgi");
    } else if policy
        .principals
        .get("amaterasu.capture-pipewire")
        .is_some_and(|grants| {
            grants
                .native_provide
                .contains("amaterasu.capture-pipewire/1")
        })
        && policy
            .settings
            .get("amaterasu.capture-pipewire")
            .is_some_and(|settings| {
                settings["enabled"] == true && settings["allow_desktop_outputs"] == true
            })
    {
        // This is authorization and provider selection. The consumer must still
        // probe that exact provider's compiled/runtime support before effects.
        exact_routes["desktop_output"] = json!("pipewire");
    }
    if let Some(settings) = policy.settings.get_mut("amaterasu.nvhttp") {
        settings["session_presentation_provider"] = json!(session_presentation::SERVICE);
        settings["auxiliary_black_capture_provider"] = json!("black");
        settings["auxiliary_exact_capture_provider"] = json!(exact_capture);
        settings["auxiliary_exact_capture_routes"] = exact_routes.clone();
    }
    for video in ["amaterasu.video", "amaterasu.linux-session-video"] {
        if let Some(settings) = policy.settings.get_mut(video) {
            if settings.get("external_capture_providers").is_none() {
                settings["external_capture_providers"] = json!({});
            }
            settings["external_capture_providers"]["black"] = json!({
                "service":"amaterasu.capture-black/1",
                "frame_transport":"generic_cpu",
            });
            let mut preflight_routes = serde_json::Map::new();
            for route in ["managed_output", "desktop_output"] {
                if let Some(provider) = exact_routes.get(route) {
                    preflight_routes
                        .insert(route.into(), json!({"provider":provider,"class":route}));
                }
            }
            preflight_routes.insert(
                "synthetic_black".into(),
                json!({"provider":"black","class":"synthetic"}),
            );
            settings["source_preflight_routes"] = serde_json::Value::Object(preflight_routes);
            settings["preflight_only_principals"] = json!([COMPONENT.id]);
        }
    }
}

fn approve_protocol_methods(policy: &mut RuntimeConfig) -> Result<(), String> {
    for method in session_presentation::methods() {
        let approval = ApiMethodApproval {
            provider: COMPONENT.id.into(),
            callers: ["amaterasu.nvhttp".into()].into(),
            method,
        };
        if let Some(existing) = policy.api_methods.iter().find(|entry| {
            entry.method.service == approval.method.service
                && entry.method.method == approval.method.method
                && entry.method.interface_major == approval.method.interface_major
        }) {
            if existing != &approval {
                return Err("selected session presentation contract conflicts with policy".into());
            }
        } else {
            policy.api_methods.push(approval);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
