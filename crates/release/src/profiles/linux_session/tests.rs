use super::*;
use crate::profiles::PackageProfile;

#[test]
fn default_extension_metadata_builds_a_disabled_generic_lifecycle() {
    let root = local_parity::TemporaryRoot::new("default-extensions");
    let policy = linux_session_policy(root.path(), &LinuxSessionOptions::default()).unwrap();
    let settings = &policy.settings["amaterasu.session-lifecycle"];
    assert_eq!(settings["capture_binding_mode"], "external");
    assert_eq!(settings["participants"].as_array().unwrap().len(), 1);
    assert_eq!(
        settings["participants"][0]["settings"]["frame_limiter_enable"],
        false
    );
    assert_eq!(
        settings["participants"][0]["plan_method"]["service"],
        "nonary.frame-limiter/1"
    );
    let registry = &policy.settings["amaterasu.extension-registry"];
    assert_eq!(
        registry["descriptors"][0]["descriptor"]["provider"],
        settings["participants"][0]["provider"]
    );
    assert!(
        registry["descriptors"][0]["descriptor"]["options"]
            .as_array()
            .unwrap()
            .len()
            > 5
    );
    let grants = registry["descriptors"][0]["grants"].as_array().unwrap();
    for kind in [
        "host",
        "all_applications",
        "all_paired_clients",
        "all_sessions",
    ] {
        assert!(
            grants
                .iter()
                .any(|grant| grant["caller"] == "amaterasu.session-lifecycle"
                    && grant["scope"]["kind"] == kind
                    && grant["read"] == true
                    && grant["write"] == false)
        );
        assert!(
            grants
                .iter()
                .any(|grant| grant["caller"] == "amaterasu.management"
                    && grant["scope"]["kind"] == kind
                    && grant["read"] == true
                    && grant["write"] == true)
        );
    }
    let replacement = linux_session_policy(
        root.path(),
        &LinuxSessionOptions {
            external_extension_composition: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        !replacement
            .settings
            .contains_key("amaterasu.session-lifecycle")
    );
    assert!(
        !replacement
            .principals
            .contains_key("amaterasu.extension-registry")
    );
}

#[test]
fn linux_launch_environment_is_an_explicit_desktop_allowlist() {
    let options = LinuxSessionOptions::default();
    let directories = PrivateStateDirectories {
        state: "/tmp/amaterasu-state".into(),
        session_history_database: "/tmp/amaterasu-state/history.db".into(),
        lifecycle: "/tmp/amaterasu-state/session-lifecycle".into(),
        rtx_hdr_runtime_parent: "/tmp/amaterasu-state/rtx-hdr".into(),
    };
    let configured = settings(&options, &directories);
    let names = configured["amaterasu.launch"]["inherit_environment"]
        .as_array()
        .unwrap();
    for required in [
        "HOME",
        "PATH",
        "DISPLAY",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
    ] {
        assert!(names.iter().any(|name| name == required));
    }
    assert!(
        !names
            .iter()
            .any(|name| name == "LD_PRELOAD" || name == "LD_LIBRARY_PATH")
    );
    assert!(configured["amaterasu.launch"].get("environment").is_none());
}

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
        streaming_settings(&options)["capture"]["capture_provider"],
        "vkms"
    );
    let physical = LinuxSessionOptions {
        capture_output: "card0-HDMI-A-1".into(),
        ..options
    };
    assert_eq!(
        streaming_settings(&physical)["capture"]["capture_provider"],
        "kms"
    );
    assert_eq!(
        streaming_settings(&physical)["capture"]["output"],
        "card0-HDMI-A-1"
    );
    assert_eq!(
        streaming_settings(&LinuxSessionOptions::default())["capture"]["output"],
        "portal-selected-session-source"
    );
}

#[test]
fn owned_display_defaults_to_registered_plugin_without_implicitly_granting_authority() {
    let options = LinuxSessionOptions {
        enable_capture: true,
        enable_virtual_display: true,
        ..Default::default()
    };
    let root = local_parity::TemporaryRoot::new("default-virtual-screen");
    let policy = linux_session_policy(root.path(), &options).unwrap();
    assert_eq!(
        policy.settings["amaterasu.nvhttp"]["streaming"]["capture"]["capture_provider"],
        "vkms"
    );
    assert_eq!(
        policy.settings[VIDEO_COMPONENT_ID]["capture_providers"],
        json!(["kms", "pipewire", "vkms"])
    );
    assert_eq!(policy.settings["amaterasu.capture-vkms"]["enabled"], false);
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

#[test]
fn opt_in_virtual_screen_alias_matches_bootstrap_and_retains_provider_grants() {
    let parse = |arguments: &[&str]| {
        LinuxSessionOptions::from_arguments(
            &arguments
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>(),
        )
    };
    let helper = "/usr/libexec/amaterasu/amaterasu-vkms-frame-helper";
    let options = parse(&[
        "--enable-capture",
        "--enable-virtual-display",
        "--frame-helper",
        helper,
        "--enable-virtual-screen-capture",
    ])
    .unwrap();
    let root = local_parity::TemporaryRoot::new("virtual-screen-alias");
    let policy = linux_session_policy(root.path(), &options).unwrap();
    let video = &policy.settings[VIDEO_COMPONENT_ID];
    assert_eq!(video["capture_providers"], json!(["kms", "pipewire"]));
    assert_eq!(
        video["external_capture_providers"]["virtual_screen"],
        json!({
            "service":"amaterasu.capture-vkms/1", "frame_transport":"dmabuf"
        })
    );
    let bootstrap = &policy.settings["amaterasu.nvhttp"]["streaming"]["capture"];
    assert_eq!(bootstrap["capture_provider"], "virtual_screen");
    assert_eq!(bootstrap["output"], "amaterasu-owned-display");
    assert_eq!(
        policy.settings["amaterasu.capture-vkms"]["frame_helper"],
        helper
    );
    let video_component = COMPONENTS
        .iter()
        .find(|component| component.id == VIDEO_COMPONENT_ID)
        .unwrap();
    assert!(video_component.calls.contains(&"amaterasu.capture-vkms/1"));
    assert!(
        video_component
            .native_uses
            .contains(&"amaterasu.capture-vkms/1")
    );
    assert!(
        video_component
            .dependencies
            .contains(&"amaterasu.capture-vkms")
    );
    for id in [NVHTTP.id, RTSP.id, STREAM_SESSION.id] {
        assert!(
            COMPONENTS
                .iter()
                .find(|component| component.id == id)
                .unwrap()
                .calls
                .contains(&"amaterasu.capture-vkms/1")
        );
    }
    assert_eq!(
        streaming_settings(&LinuxSessionOptions::default())["capture"]["capture_provider"],
        "pipewire"
    );

    for arguments in [
        vec!["--enable-virtual-screen-capture"],
        vec![
            "--enable-capture",
            "--enable-virtual-display",
            "--enable-virtual-screen-capture",
        ],
        vec![
            "--enable-capture",
            "--frame-helper",
            helper,
            "--enable-virtual-screen-capture",
        ],
        vec![
            "--enable-virtual-display",
            "--frame-helper",
            helper,
            "--enable-virtual-screen-capture",
        ],
        vec![
            "--enable-capture",
            "--enable-virtual-display",
            "--frame-helper",
            helper,
            "--capture-output",
            "kms:card0",
            "--enable-virtual-screen-capture",
        ],
        vec![
            "--enable-capture",
            "--enable-virtual-display",
            "--frame-helper",
            helper,
            "--enable-virtual-screen-capture",
            "--enable-virtual-screen-capture",
        ],
    ] {
        assert!(parse(&arguments).is_err(), "{arguments:?}");
    }
    let invalid = LinuxSessionOptions {
        enable_virtual_screen_capture: true,
        ..Default::default()
    };
    assert!(linux_session_policy(root.path(), &invalid).is_err());
}

#[test]
fn package_capture_authority_requires_scoped_owned_display_options() {
    let parse = |values: &[&str]| LinuxSessionOptions::from_arguments(&arguments(values));
    let helper = "/opt/amaterasu/artifacts/amaterasu-vkms-frame-helper";
    let base = [
        "--enable-capture",
        "--enable-virtual-display",
        "--enable-virtual-screen-capture",
        "--capture-authority-helper",
        helper,
    ];
    let options = parse(&base).unwrap();
    assert_eq!(options.capture_authority_helper.as_deref(), Some(helper));
    assert_eq!(options.capture_authority_card, 0);
    assert!(options.frame_helper.is_none());

    let default_options = parse(&[
        "--enable-capture",
        "--enable-virtual-display",
        "--capture-authority-helper",
        helper,
        "--capture-authority-card",
        "3",
    ])
    .unwrap();
    assert_eq!(
        streaming_settings(&default_options)["capture"]["capture_provider"],
        "vkms"
    );
    assert_eq!(default_options.capture_authority_card, 3);
    assert_eq!(
        vkms_capture_settings(&default_options)["authority_channel"],
        true
    );

    let mut scoped = base.to_vec();
    scoped.extend(["--capture-authority-card", "63"]);
    assert_eq!(parse(&scoped).unwrap().capture_authority_card, 63);

    for values in [
        vec!["--capture-authority-helper", helper],
        vec!["--enable-capture", "--capture-authority-helper", helper],
        vec![
            "--enable-virtual-display",
            "--capture-authority-helper",
            helper,
        ],
        vec![
            "--enable-capture",
            "--enable-virtual-display",
            "--capture-output",
            "card0-HDMI-A-1",
            "--capture-authority-helper",
            helper,
        ],
        vec!["--capture-authority-card", "0"],
        vec!["--capture-authority-card", "1"],
        vec!["--frame-helper", helper, "--capture-authority-card", "0"],
        vec![
            "--enable-virtual-screen-capture",
            "--capture-authority-helper",
            helper,
        ],
        vec![
            "--enable-capture",
            "--enable-virtual-screen-capture",
            "--capture-authority-helper",
            helper,
        ],
        vec![
            "--enable-virtual-display",
            "--enable-virtual-screen-capture",
            "--capture-authority-helper",
            helper,
        ],
    ] {
        assert!(parse(&values).is_err(), "accepted {values:?}");
    }

    for extra in [
        vec!["--frame-helper", helper],
        vec!["--capture-output", "kms:card0"],
        vec!["--capture-authority-helper", helper],
        vec![
            "--capture-authority-card",
            "1",
            "--capture-authority-card",
            "2",
        ],
    ] {
        let mut values = base.to_vec();
        values.extend(extra);
        assert!(parse(&values).is_err(), "accepted {values:?}");
    }
}

#[test]
fn package_capture_authority_rejects_invalid_paths_and_card_scope() {
    let parse = |values: &[&str]| LinuxSessionOptions::from_arguments(&arguments(values));
    let base = [
        "--enable-capture",
        "--enable-virtual-display",
        "--enable-virtual-screen-capture",
    ];
    for path in [
        "",
        "relative/helper",
        "/opt/../tmp/helper",
        "/opt/helpers/",
        "/",
    ] {
        let mut values = base.to_vec();
        values.extend(["--capture-authority-helper", path]);
        assert!(
            matches!(
                parse(&values),
                Err(options::OptionError::InvalidCaptureAuthorityHelper(_))
            ),
            "accepted {path:?}"
        );
    }
    let mut with_helper = base.to_vec();
    with_helper.extend(["--capture-authority-helper", "/opt/amaterasu/helper"]);
    for card in ["64", "4294967296", "-1", "", "one", "1.0"] {
        let mut values = with_helper.clone();
        values.extend(["--capture-authority-card", card]);
        assert!(
            matches!(
                parse(&values),
                Err(options::OptionError::InvalidCaptureAuthorityCard(_))
            ),
            "accepted {card:?}"
        );
    }
    for option in ["--capture-authority-helper", "--capture-authority-card"] {
        let mut values = base.to_vec();
        values.push(option);
        assert!(matches!(
            parse(&values),
            Err(options::OptionError::MissingValue(_))
        ));
    }
}

#[test]
fn programmatic_capture_authority_options_preserve_validation() {
    let valid = LinuxSessionOptions {
        enable_capture: true,
        enable_virtual_display: true,
        enable_virtual_screen_capture: true,
        capture_authority_helper: Some("/opt/amaterasu/helper".into()),
        capture_authority_card: 3,
        ..Default::default()
    };
    assert!(valid.validate().is_ok());
    assert!(
        LinuxSessionOptions {
            capture_authority_card: 64,
            ..valid.clone()
        }
        .validate()
        .is_err()
    );
    assert!(
        LinuxSessionOptions {
            capture_authority_helper: None,
            ..valid.clone()
        }
        .validate()
        .is_err()
    );
    assert!(
        LinuxSessionOptions {
            frame_helper: Some("/opt/legacy/helper".into()),
            ..valid.clone()
        }
        .validate()
        .is_err()
    );
    assert!(
        LinuxSessionOptions {
            enable_virtual_screen_capture: false,
            ..valid.clone()
        }
        .validate()
        .is_ok()
    );
    assert!(
        LinuxSessionOptions {
            enable_virtual_screen_capture: false,
            enable_capture: false,
            ..valid.clone()
        }
        .validate()
        .is_err()
    );
    assert!(
        LinuxSessionOptions {
            capture_authority_helper: Some("relative/helper".into()),
            ..valid
        }
        .validate()
        .is_err()
    );
}

#[test]
fn package_authority_profile_uses_only_the_inherited_channel() {
    let options = LinuxSessionOptions {
        enable_capture: true,
        enable_virtual_display: true,
        enable_virtual_screen_capture: true,
        capture_authority_helper: Some("/build/artifacts/amaterasu-vkms-frame-helper".into()),
        capture_authority_card: 3,
        ..LinuxSessionOptions::default()
    };
    options.validate().unwrap();
    assert_eq!(
        vkms_capture_settings(&options),
        json!({
            "enabled": true,
            "capture_principals": [VIDEO_COMPONENT_ID],
            "authority_channel": true,
            "gpu_snapshot": false,
        })
    );
    assert!(
        vkms_capture_settings(&options)
            .get("authority_channel_fd")
            .is_none()
    );
    assert!(
        vkms_capture_settings(&options)
            .get("frame_helper")
            .is_none()
    );
}

#[test]
fn session_extensions_profile_adds_actions_to_default_extension_services() {
    let extra = [
        (
            "amaterasu.session-lifecycle",
            "amaterasu.session-lifecycle/1",
        ),
        (
            "amaterasu.extension-registry",
            "amaterasu.extension-registry/1",
        ),
        (
            "amaterasu.extension-actions",
            "amaterasu.extension-actions/1",
        ),
    ];
    for (id, service) in extra {
        assert_eq!(
            COMPONENTS.iter().any(|component| component.id == id),
            id != "amaterasu.extension-actions"
        );
        let component = EXTENSION_COMPONENTS
            .iter()
            .find(|component| component.id == id)
            .unwrap();
        assert!(component.provides.contains(&service));
        assert!(component.dependencies.contains(&RUNTIME_ID));
    }
    assert_eq!(EXTENSION_COMPONENTS.len(), COMPONENTS.len() + 1);
    assert!(COMPONENTS.iter().all(|component| {
        EXTENSION_COMPONENTS
            .iter()
            .any(|candidate| candidate.id == component.id)
    }));
}

#[test]
fn rtx_hdr_composition_is_explicit_and_authorizes_the_processor() {
    let root = TemporaryRoot::new("rtx-hdr-composition");
    let default = linux_session_policy(root.path(), &LinuxSessionOptions::default()).unwrap();
    assert!(!default.principals.contains_key("amaterasu.rtx-hdr"));
    assert!(!default.settings.contains_key("amaterasu.rtx-hdr"));
    assert!(
        !default.principals[VIDEO_COMPONENT_ID]
            .native_use
            .contains("amaterasu.rtx-hdr/1")
    );
    assert!(
        COMPONENTS
            .iter()
            .all(|component| component.id != "amaterasu.rtx-hdr")
    );

    let runtime = root.path().join("runtime");
    let enabled = linux_session_policy(
        root.path(),
        &LinuxSessionOptions {
            rtx_hdr_runtime_directory: Some(runtime.clone()),
            frame_helper: Some("/opt/amaterasu/amaterasu-vkms-frame-helper".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let provider = RTX_HDR_COMPONENTS
        .iter()
        .find(|component| component.id == "amaterasu.rtx-hdr")
        .unwrap();
    let video = RTX_HDR_COMPONENTS
        .iter()
        .find(|component| component.id == VIDEO_COMPONENT_ID)
        .unwrap();
    assert!(provider.native_provides.contains(&"amaterasu.rtx-hdr/1"));
    assert!(video.native_uses.contains(&"amaterasu.rtx-hdr/1"));
    assert!(video.dependencies.contains(&provider.id));
    assert!(video.calls.contains(&"amaterasu.session-lifecycle/1"));
    assert!(video.dependencies.contains(&"amaterasu.session-lifecycle"));
    assert!(
        enabled.principals[VIDEO_COMPONENT_ID]
            .call
            .contains("amaterasu.session-lifecycle/1")
    );
    assert!(
        enabled.principals[VIDEO_COMPONENT_ID]
            .native_use
            .contains("amaterasu.rtx-hdr/1")
    );
    assert!(
        enabled.principals[provider.id]
            .native_provide
            .contains("amaterasu.rtx-hdr/1")
    );
    assert_eq!(
        enabled.settings[VIDEO_COMPONENT_ID]["gpu_processor_service"],
        "amaterasu.rtx-hdr/1"
    );
    assert_eq!(
        enabled.settings[provider.id]["runtime_directory"],
        json!(runtime)
    );
    assert!(
        enabled.settings[VIDEO_COMPONENT_ID]
            .get("require_frame_lease")
            .is_none()
    );
    assert_eq!(
        enabled.settings[VIDEO_COMPONENT_ID]["gpu_processor_opt_in"],
        true
    );
    assert_eq!(
        enabled.settings["amaterasu.capture-vkms"]["gpu_snapshot"],
        true
    );
    assert_eq!(
        enabled.settings["amaterasu.capture-vkms"]["require_lease"],
        true
    );
    assert_eq!(
        enabled.settings["amaterasu.stream-session"]["processing_plan"]["provider"],
        "amaterasu.rtx-hdr"
    );
    assert_eq!(
        enabled.settings["amaterasu.stream-session"]["processing_plan"]["omit_disabled"],
        true
    );
    assert_eq!(
        enabled.settings["amaterasu.session-lifecycle"]["processing_policy"]["processor_service"],
        "amaterasu.rtx-hdr/1"
    );
    assert!(enabled.settings["amaterasu.session-lifecycle"]["provider_settings_readers"]["amaterasu.stream-session"].as_array().unwrap().contains(&json!("amaterasu.rtx-hdr")));
    let lifecycle = &enabled.settings["amaterasu.session-lifecycle"];
    assert!(
        lifecycle["trusted_callers"]
            .as_array()
            .unwrap()
            .contains(&json!(VIDEO_COMPONENT_ID))
    );
    assert!(
        lifecycle["operation_grants"][VIDEO_COMPONENT_ID]
            .as_array()
            .unwrap()
            .contains(&json!("read_live_provider_settings"))
    );
    assert!(
        lifecycle["provider_settings_readers"][VIDEO_COMPONENT_ID]
            .as_array()
            .unwrap()
            .contains(&json!("amaterasu.rtx-hdr"))
    );
    assert!(
        default.settings["amaterasu.session-lifecycle"]["provider_settings_readers"]
            .get(VIDEO_COMPONENT_ID)
            .is_none()
    );
    assert!(
        enabled.settings["amaterasu.session-lifecycle"]["participants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|participant| participant["provider"] == "amaterasu.rtx-hdr")
    );
    assert!(
        enabled.settings["amaterasu.extension-registry"]["descriptors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|descriptor| descriptor["descriptor"]["provider"] == "amaterasu.rtx-hdr")
    );
    for approval in enabled
        .api_methods
        .iter()
        .filter(|approval| approval.provider == "amaterasu.rtx-hdr")
    {
        for caller in &approval.callers {
            assert!(
                enabled.principals[caller]
                    .call
                    .contains(&approval.method.service),
                "bootstrap lacks control grant for {caller}"
            );
            if caller != CONSOLE_ID {
                let component = RTX_HDR_COMPONENTS
                    .iter()
                    .find(|component| component.id == caller)
                    .expect("approved caller must be packaged");
                assert!(
                    component.calls.contains(&approval.method.service.as_str()),
                    "signed component lacks control grant for {caller}"
                );
            }
        }
    }
    assert!(
        enabled
            .api_methods
            .iter()
            .any(|approval| approval.provider == "amaterasu.rtx-hdr"
                && approval.method.method == "processing_intent")
    );
    assert_eq!(RTX_HDR_COMPONENTS.len(), COMPONENTS.len() + 1);
    for component in COMPONENTS {
        assert!(
            RTX_HDR_COMPONENTS
                .iter()
                .any(|entry| entry.id == component.id)
        );
    }
    assert_eq!(
        default.settings["amaterasu.management"]["rtx_hdr_runtime_parent"],
        json!(root.path().join("data/state/rtx-hdr"))
    );
}

#[test]
fn rtx_hdr_policy_conflicts_leave_existing_policy_unchanged() {
    let root = TemporaryRoot::new("rtx-hdr-conflict");
    let mut policy = linux_session_policy(root.path(), &LinuxSessionOptions::default()).unwrap();
    policy.settings.get_mut("amaterasu.capture-vkms").unwrap()["frame_helper"] =
        json!("/opt/amaterasu/amaterasu-vkms-frame-helper");
    policy.settings.get_mut(VIDEO_COMPONENT_ID).unwrap()["cpu_processor_service"] =
        json!("example.other/1");
    let before = serde_json::to_value(&policy).unwrap();
    assert!(enable_rtx_hdr(&mut policy, &root.path().join("runtime")).is_err());
    assert_eq!(serde_json::to_value(&policy).unwrap(), before);
}

#[test]
fn rtx_hdr_policy_rejects_missing_capture_helper_without_mutation() {
    let root = TemporaryRoot::new("rtx-hdr-missing-helper");
    let mut policy = linux_session_policy(root.path(), &LinuxSessionOptions::default()).unwrap();
    let before = serde_json::to_value(&policy).unwrap();
    let error = enable_rtx_hdr(&mut policy, &root.path().join("runtime")).unwrap_err();
    assert!(error.contains("frame helper"));
    assert_eq!(serde_json::to_value(&policy).unwrap(), before);
}
