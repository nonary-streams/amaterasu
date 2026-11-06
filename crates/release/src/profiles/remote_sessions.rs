//! Trusted product assembly for replaceable client-session presentation.
//!
//! The protocol consumer selects the public service explicitly. Product-specific
//! controls and retained ownership stay in the selected native plugin.
use super::{ComponentDefinition, RUNTIME_ID, X11_VIDEO, platform_extensions};
use amaterasu_contracts::{ApiMethodApproval, RuntimeConfig};
use amaterasu_module_sdk::session_presentation;
use serde_json::json;

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
    ],
    publishes: &[],
    dependencies: &[
        RUNTIME_ID,
        "amaterasu.configuration",
        "amaterasu.state",
        "amaterasu.catalog",
        "amaterasu.virtual-display",
        "amaterasu.display",
        "amaterasu.rtsp",
    ],
    native_provides: &[],
    native_uses: &[],
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
    if let Some(settings) = policy.settings.get_mut("amaterasu.rtsp") {
        settings["retirement_status_principals"] = json!([COMPONENT.id]);
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
    if let Some(settings) = policy.settings.get_mut("amaterasu.nvhttp") {
        settings["session_presentation_provider"] = json!(session_presentation::SERVICE);
        settings["auxiliary_black_capture_provider"] = json!("black");
        settings["auxiliary_exact_capture_provider"] = json!(exact_capture);
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
