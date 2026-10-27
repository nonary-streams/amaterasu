use super::*;
use crate::profiles::PackageProfile;

#[test]
fn profile_uses_owner_selected_pipewire_capture_alongside_kms_metrics_and_library_integrations() {
    let capture = COMPONENTS
        .iter()
        .find(|component| component.id == "amaterasu.capture-pipewire")
        .expect("PipeWire capture component");
    assert_eq!(capture.library, "amaterasu_capture_pipewire");
    assert!(
        capture
            .native_provides
            .contains(&"amaterasu.capture-pipewire/1")
    );
    assert!(capture.dependencies.contains(&RUNTIME_ID));

    let video = COMPONENTS
        .iter()
        .find(|component| component.id == VIDEO_COMPONENT_ID)
        .expect("Linux video component");
    assert!(video.calls.contains(&"amaterasu.capture-pipewire/1"));
    assert!(video.native_uses.contains(&"amaterasu.capture-pipewire/1"));
    assert!(video.dependencies.contains(&"amaterasu.capture-pipewire"));

    // Nested broker requests intersect the call grants at every ancestor, but
    // only video needs direct native-use authority for frame capture.
    for component_id in [NVHTTP.id, RTSP.id, STREAM_SESSION.id] {
        let component = COMPONENTS
            .iter()
            .find(|component| component.id == component_id)
            .expect("stream ancestor component");
        assert!(component.calls.contains(&"amaterasu.capture-pipewire/1"));
        assert!(
            !component
                .native_uses
                .contains(&"amaterasu.capture-pipewire/1")
        );
    }

    let management = COMPONENTS
        .iter()
        .find(|component| component.id == MANAGEMENT.id)
        .expect("Linux management component");
    assert!(management.calls.contains(&"amaterasu.video/1"));
    assert!(management.calls.contains(&"amaterasu.capture-pipewire/1"));
    assert!(
        !management
            .native_uses
            .contains(&"amaterasu.capture-pipewire/1")
    );
    assert!(
        !management
            .dependencies
            .contains(&"amaterasu.capture-pipewire")
    );
    assert!(ALLOWED_SCOPES.contains(&"capture.read"));
    assert!(ALLOWED_SCOPES.contains(&"capture.write"));
    assert_eq!(
        portal_capture_settings(),
        json!({
            "enabled": true,
            "selection_principals": [VIDEO_COMPONENT_ID, MANAGEMENT.id],
            "capture_principals": [VIDEO_COMPONENT_ID],
        })
    );
    assert_eq!(
        streaming_settings(&LinuxSessionOptions::default())["capture"]["capture_provider"],
        "pipewire"
    );
    assert_eq!(
        streaming_settings(&LinuxSessionOptions::default())["desktop"]["enabled"],
        true
    );
    assert_eq!(
        catalog_settings(&LinuxSessionOptions::default())["seed_desktop_entry"],
        false
    );
    assert_eq!(
        catalog_settings(&LinuxSessionOptions::default())["import_principals"]["amaterasu.steam"],
        "amaterasu.management"
    );
    assert!(ALLOWED_SCOPES.contains(&"steam.read"));
    assert!(ALLOWED_SCOPES.contains(&"lutris.read"));
    assert!(
        PackageProfile::Platform
            .components()
            .iter()
            .any(|component| component.id == "amaterasu.steam")
    );
    assert!(
        PackageProfile::Platform
            .components()
            .iter()
            .any(|component| component.id == "amaterasu.lutris")
    );
    assert!(
        COMPONENTS
            .iter()
            .any(|component| component.id == "amaterasu.capture-kms")
    );
    assert!(
        COMPONENTS
            .iter()
            .any(|component| component.id == "amaterasu.host-metrics")
    );
    assert!(
        !PackageProfile::Platform
            .components()
            .iter()
            .any(|component| component.id == capture.id)
    );
}

#[test]
fn management_delegates_portal_selection_to_video() {
    let management = COMPONENTS
        .iter()
        .find(|component| component.id == MANAGEMENT.id)
        .expect("Linux management component");
    let video = COMPONENTS
        .iter()
        .find(|component| component.id == VIDEO_COMPONENT_ID)
        .expect("Linux video component");

    assert!(management.calls.contains(&"amaterasu.video/1"));
    // This is a transitive broker grant for management -> video -> provider,
    // not a native-use grant or a direct provider dependency.
    assert!(management.calls.contains(&"amaterasu.capture-pipewire/1"));
    assert!(
        !management
            .dependencies
            .contains(&"amaterasu.capture-pipewire")
    );
    assert!(video.calls.contains(&"amaterasu.capture-pipewire/1"));
    assert!(video.native_uses.contains(&"amaterasu.capture-pipewire/1"));
    assert_eq!(
        portal_capture_settings()["selection_principals"],
        json!([VIDEO_COMPONENT_ID, MANAGEMENT.id])
    );
}

#[test]
fn stream_session_opens_portal_capture_only_through_video() {
    let stream_session = COMPONENTS
        .iter()
        .find(|component| component.id == STREAM_SESSION.id)
        .expect("Linux stream-session component");
    let video = COMPONENTS
        .iter()
        .find(|component| component.id == VIDEO_COMPONENT_ID)
        .expect("Linux video component");

    assert!(stream_session.calls.contains(&"amaterasu.video/1"));
    assert!(
        stream_session
            .calls
            .contains(&"amaterasu.capture-pipewire/1")
    );
    assert!(
        !stream_session
            .native_uses
            .contains(&"amaterasu.capture-pipewire/1")
    );
    assert!(video.native_uses.contains(&"amaterasu.capture-pipewire/1"));
    assert_eq!(
        portal_capture_settings()["capture_principals"],
        json!([VIDEO_COMPONENT_ID])
    );
}

#[test]
fn signed_linux_session_policy_advertises_reserved_desktop_without_catalog_row() {
    let root = std::env::temp_dir().join(format!(
        "amaterasu-linux-session-policy-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);

    let policy = linux_session_policy(&root, &LinuxSessionOptions::default()).unwrap();
    assert_eq!(
        PackageProfile::LinuxSession.components().len(),
        COMPONENTS.len()
    );
    assert_eq!(
        policy.settings["amaterasu.nvhttp"]["streaming"]["desktop"]["enabled"],
        true
    );
    assert_eq!(
        policy.settings["amaterasu.catalog"]["seed_desktop_entry"],
        false
    );
    assert!(
        COMPONENTS
            .iter()
            .any(|component| matches!(component.id, "amaterasu.steam" | "amaterasu.lutris"))
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn linux_profile_selects_encoders_automatically_for_the_negotiated_codec() {
    let encoder = &streaming_settings(&LinuxSessionOptions::default())["capture"]["encoder"];
    assert_eq!(encoder, &json!("auto"));
}

#[test]
fn linux_profile_allows_hardware_modes_up_to_uhd_at_120_hz() {
    let profile = &streaming_settings(&LinuxSessionOptions::default())["profile"];
    assert_eq!(profile["max_width"], 3840);
    assert_eq!(profile["max_height"], 2160);
    assert_eq!(profile["max_fps"], 120);
    assert_eq!(profile["bitrate_kbps"], 100_000);
    // The legacy single-codec field stays absent: codecs come from encoders.
    assert!(profile.get("codec").is_none());
}

#[test]
fn linux_profile_keeps_encrypted_audio_playing_across_sequence_wrap() {
    let root = std::env::temp_dir().join("amaterasu-audio-wrap-policy");
    let policy = linux_session_policy(&root, &LinuxSessionOptions::default()).unwrap();
    let profile = &policy.settings["amaterasu.nvhttp"]["streaming"]["profile"];
    assert_eq!(profile["encrypt_audio"], true);
    assert_eq!(profile["permit_legacy_audio_iv_wrap"], true);
}

#[test]
fn explicitly_enabled_virtual_displays_select_a_stream_owned_output() {
    let options = LinuxSessionOptions {
        enable_virtual_display: true,
        ..Default::default()
    };
    assert_eq!(
        streaming_settings(&options)["capture"]["output"],
        "amaterasu-owned-display"
    );
    assert_eq!(
        streaming_settings(&LinuxSessionOptions::default())["capture"]["output"],
        "portal-selected-session-source"
    );
}

#[test]
fn video_may_capture_owned_outputs_through_the_vkms_scanout_provider() {
    let capture = COMPONENTS
        .iter()
        .find(|component| component.id == "amaterasu.capture-vkms")
        .expect("vkms capture component");
    assert_eq!(capture.library, "amaterasu_capture_vkms");
    assert!(
        capture
            .native_provides
            .contains(&"amaterasu.capture-vkms/1")
    );

    let video = COMPONENTS
        .iter()
        .find(|component| component.id == VIDEO_COMPONENT_ID)
        .expect("Linux video component");
    assert!(video.calls.contains(&"amaterasu.capture-vkms/1"));
    assert!(video.native_uses.contains(&"amaterasu.capture-vkms/1"));
    assert!(video.dependencies.contains(&"amaterasu.capture-vkms"));

    // Every ancestor of video's capture requests carries the call grant so the
    // intersected authority still reaches the provider; none gets native use.
    for component_id in [NVHTTP.id, RTSP.id, STREAM_SESSION.id, MANAGEMENT.id] {
        let component = COMPONENTS
            .iter()
            .find(|component| component.id == component_id)
            .expect("stream ancestor component");
        assert!(component.calls.contains(&"amaterasu.capture-vkms/1"));
        assert!(!component.native_uses.contains(&"amaterasu.capture-vkms/1"));
    }
}

#[test]
fn vkms_capture_stays_disabled_until_a_frame_helper_is_named() {
    let without_helper = LinuxSessionOptions {
        enable_capture: true,
        ..Default::default()
    };
    assert_eq!(
        vkms_capture_settings(&without_helper),
        json!({"enabled": false})
    );

    let helper = "/usr/local/libexec/amaterasu/amaterasu-vkms-frame-helper";
    let with_helper = LinuxSessionOptions {
        enable_capture: true,
        frame_helper: Some(helper.into()),
        ..Default::default()
    };
    assert_eq!(
        vkms_capture_settings(&with_helper),
        json!({
            "enabled": true,
            "capture_principals": [VIDEO_COMPONENT_ID],
            "frame_helper": helper,
        })
    );
    let capture_disabled = LinuxSessionOptions {
        frame_helper: Some(helper.into()),
        ..Default::default()
    };
    assert_eq!(vkms_capture_settings(&capture_disabled)["enabled"], false);
}

#[test]
fn frame_helper_option_requires_one_absolute_normal_path() {
    let parse = |values: &[&str]| {
        LinuxSessionOptions::from_arguments(
            &values
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>(),
        )
    };
    let helper = "/usr/local/libexec/amaterasu/amaterasu-vkms-frame-helper";
    assert_eq!(
        parse(&["--frame-helper", helper])
            .unwrap()
            .frame_helper
            .as_deref(),
        Some(helper)
    );
    for invalid in ["relative/helper", "/usr/../tmp/helper", "/usr/libexec/", ""] {
        assert!(parse(&["--frame-helper", invalid]).is_err(), "{invalid:?}");
    }
    assert!(parse(&["--frame-helper"]).is_err());
    assert!(parse(&["--frame-helper", helper, "--frame-helper", helper]).is_err());
}

#[test]
fn owned_displays_are_exclusive_and_automatically_scaled() {
    let root = std::env::temp_dir().join(format!(
        "amaterasu-linux-session-display-policy-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let options = LinuxSessionOptions {
        enable_virtual_display: true,
        ..Default::default()
    };
    let policy = linux_session_policy(&root, &options).unwrap();
    let settings = &policy.settings["amaterasu.virtual-display"];
    assert_eq!(settings["enabled"], true);
    assert_eq!(settings["virtual_display_layout"], "exclusive");
    assert_eq!(settings["dd_virtual_display_scale"], -1);
    std::fs::remove_dir_all(root).unwrap();
}

mod local_parity;
use local_parity::{TemporaryRoot, arguments, default_policy};
mod network;

#[test]
fn explicit_kms_output_preserves_operator_capture_selection() {
    let options = LinuxSessionOptions::from_arguments(&[
        "--capture-output".into(),
        "kms:card1/conn=3/crtc=4/plane=5".into(),
        "--enable-capture".into(),
    ])
    .unwrap();
    let capture = &streaming_settings(&options)["capture"];
    assert_eq!(capture["capture_provider"], "kms");
    assert_eq!(capture["output"], "kms:card1/conn=3/crtc=4/plane=5");
}

#[test]
fn portal_selection_authorizes_direct_management_and_video_ensure_routes() {
    let selection = portal_capture_settings()["selection_principals"]
        .as_array()
        .unwrap()
        .clone();
    assert!(selection.contains(&json!(MANAGEMENT.id)));
    assert!(selection.contains(&json!(VIDEO_COMPONENT_ID)));
    for component in [LINUX_SESSION_MANAGEMENT, LINUX_SESSION_VIDEO] {
        assert!(component.calls.contains(&"amaterasu.capture-pipewire/1"));
    }
    assert!(
        !LINUX_SESSION_MANAGEMENT
            .native_uses
            .contains(&"amaterasu.capture-pipewire/1")
    );
    assert_eq!(
        portal_capture_settings()["capture_principals"],
        json!([VIDEO_COMPONENT_ID])
    );
}
