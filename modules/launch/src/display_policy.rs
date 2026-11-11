//! Sparse display settings carried through launch admission. Native effects
//! remain owned by the selected display provider.
use serde_json::Value;

pub fn is_global_only_key(key: &str) -> bool {
    matches!(
        key,
        "dd_remove_virtual_display_after_restore"
            | "dd_wa_hdr_toggle"
            | "dd_wa_hdr_toggle_delay"
            | "vulkan_hdr_layer"
            | "wayland_hdr_compatibility"
    )
}

/// `None` identifies the original typed mode-policy fields, validated by the
/// launch protocol. Every newer field is validated here before attestation.
pub fn valid_additional_field(key: &str, value: &Value) -> Option<bool> {
    let text = |limit| {
        value
            .as_str()
            .is_some_and(|text| text.len() <= limit && !text.chars().any(char::is_control))
    };
    Some(match key {
        "adapter_name" => text(1024),
        "adapter_pnp_id" => text(256),
        "virtual_display_mode" => value
            .as_str()
            .is_some_and(|mode| matches!(mode, "disabled" | "per_client" | "shared")),
        "virtual_display_layout" => value.as_str().is_some_and(|layout| {
            matches!(
                layout,
                "exclusive"
                    | "extended"
                    | "extended_primary"
                    | "extended_isolated"
                    | "extended_primary_isolated"
            )
        }),
        "dd_config_revert_delay" | "dd_paused_virtual_display_timeout_secs" => value
            .as_u64()
            .is_some_and(|number| number <= i32::MAX as u64),
        "dd_config_revert_on_disconnect"
        | "dd_remove_virtual_display_after_restore"
        | "dd_always_restore_from_golden"
        | "dd_use_sunshine_virtual_display_driver"
        | "dd_activate_virtual_display"
        | "dd_wa_dummy_plug_hdr10"
        | "dd_wa_hdr_toggle"
        | "vulkan_hdr_layer"
        | "wayland_hdr_compatibility" => value.is_boolean(),
        "dd_wa_hdr_toggle_delay" => value.as_u64().is_some_and(|delay| delay <= 3000),
        "dd_display_helper_engine" => value
            .as_str()
            .is_some_and(|engine| matches!(engine, "auto" | "v2" | "legacy")),
        "dd_snapshot_restore_hotkey" => text(64),
        "dd_snapshot_restore_hotkey_modifiers" => text(128),
        "dd_virtual_display_scale" => value.as_i64().is_some_and(|scale| {
            matches!(
                scale,
                -1 | 0 | 100 | 125 | 150 | 175 | 200 | 225 | 250 | 300 | 350 | 400 | 450 | 500
            )
        }),
        "dd_virtual_display_permanent_count" => value.as_u64().is_some_and(|count| count <= 4),
        "virtual_display_outputs" => valid_string_array(value, 32, 512),
        "dd_snapshot_exclude_devices" => valid_string_array(value, 256, 1024),
        _ => return None,
    })
}

fn valid_string_array(value: &Value, count: usize, bytes: usize) -> bool {
    value.as_array().is_some_and(|items| {
        items.len() <= count
            && items.iter().all(|item| {
                item.as_str().is_some_and(|text| {
                    !text.is_empty() && text.len() <= bytes && !text.chars().any(char::is_control)
                })
            })
    })
}
