use super::*;

#[test]
fn every_profile_contains_unique_providers_and_all_dependencies() {
    for profile in [
        PackageProfile::Example,
        PackageProfile::Platform,
        PackageProfile::MediaProbe,
        PackageProfile::StreamProbe,
    ] {
        let components = profile.components();
        let ids: BTreeSet<_> = components.iter().map(|component| component.id).collect();
        assert_eq!(ids.len(), components.len());
        let mut control = BTreeSet::new();
        let mut native = BTreeSet::new();
        for component in components {
            assert!(
                component
                    .dependencies
                    .iter()
                    .all(|dependency| ids.contains(dependency)),
                "{} dependency missing",
                component.id
            );
            for service in component.provides {
                assert!(control.insert(service), "duplicate {service}");
            }
            for service in component.native_provides {
                assert!(native.insert(service), "duplicate {service}");
            }
        }
        for component in components {
            assert!(
                component
                    .native_uses
                    .iter()
                    .all(|service| native.contains(service)),
                "{} native provider missing",
                component.id
            );
        }
    }
}

#[test]
fn stream_probe_replaces_platform_sources_without_widening_native_interfaces() {
    let probe = PackageProfile::StreamProbe.components();
    assert!(
        probe
            .iter()
            .any(|component| component.id == STREAM_SOURCE.id)
    );
    assert!(!probe.iter().any(|component| {
        [
            VIDEO.id,
            AUDIO.id,
            CAPTURE_DXGI.id,
            CAPTURE_WGC.id,
            ENCODE.id,
        ]
        .contains(&component.id)
    }));
    let session = probe
        .iter()
        .find(|component| component.id == STREAM_SESSION.id)
        .unwrap();
    assert_eq!(session.native_uses.len(), 5);
    assert_eq!(session.native_uses, STREAM_SESSION.native_uses);
    assert!(session.dependencies.contains(&STREAM_SOURCE.id));
    assert!(session.dependencies.contains(&INPUT_PROBE.id));
    assert!(probe.iter().any(|component| component.id == INPUT_PROBE.id));
    assert!(!probe.iter().any(|component| component.id == INPUT.id));
    let production = PackageProfile::Platform
        .components()
        .iter()
        .find(|component| component.id == STREAM_SESSION.id)
        .unwrap();
    assert!(production.dependencies.contains(&VIDEO.id));
    assert!(production.dependencies.contains(&AUDIO.id));
    assert!(production.dependencies.contains(&INPUT.id));
    assert!(production.calls.contains(&"amaterasu.input/1"));
    assert!(
        !PackageProfile::Platform
            .components()
            .iter()
            .any(|component| component.id == INPUT_PROBE.id)
    );
    // Delegation intersects each provider's grants, so these lower calls must
    // survive the stream-session -> video -> capture/encode chain.
    assert!(
        VIDEO
            .calls
            .iter()
            .all(|service| production.calls.contains(service))
    );
    for profile in [PackageProfile::Platform, PackageProfile::StreamProbe] {
        let rtsp = profile
            .components()
            .iter()
            .find(|component| component.id == RTSP.id)
            .unwrap();
        assert!(rtsp.dependencies.contains(&STREAM_SESSION.id));
        assert!(rtsp.calls.contains(&"amaterasu.stream-session/1"));
        assert!(
            STREAM_SESSION
                .calls
                .iter()
                .all(|service| rtsp.calls.contains(service))
        );
        assert!(rtsp.native_uses.is_empty());
    }
}

#[test]
fn development_policy_keeps_stream_session_disabled_and_console_non_native() {
    let root = std::env::temp_dir().join(format!(
        "amaterasu-release-stream-profile-{}",
        std::process::id()
    ));
    let policy = development_policy(&root).unwrap();
    let settings = &policy.settings[STREAM_SESSION.id];
    assert_eq!(settings["enabled"], false);
    assert_eq!(settings["allow_remote"], false);
    assert_eq!(settings["allowed_principals"], json!([]));
    assert_ne!(settings["enable_input"], true);
    assert_eq!(policy.settings[INPUT.id]["allow_injection"], false);
    assert_eq!(policy.settings[INPUT_PROBE.id]["enabled"], false);
    let http_settings = &policy.settings[NVHTTP.id];
    assert_eq!(http_settings["enabled"], false);
    assert!(http_settings.get("streaming").is_none());
    let settings = &policy.settings[RTSP.id];
    assert_eq!(settings["enabled"], false);
    assert_eq!(settings["allow_remote"], false);
    assert_eq!(settings["allowed_principals"], json!([]));
    assert!(
        policy.principals["amaterasu.console"]
            .call
            .contains("amaterasu.stream-session/1")
    );
    assert!(policy.principals["amaterasu.console"].native_use.is_empty());
    assert_eq!(
        policy.principals[STREAM_SESSION.id].native_use,
        names(STREAM_SESSION.native_uses)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn installer_policy_enables_only_the_loopback_control_plane() {
    let policy = installer_policy(
        "__WEB__",
        [("installer-release".into(), "11".repeat(32))].into(),
    );

    assert!(!policy.principals.contains_key("amaterasu.console"));
    assert!(!policy.settings.contains_key("amaterasu.state"));
    assert_eq!(
        policy.settings["amaterasu.management"],
        json!({
            "enabled": true,
            "bind": "127.0.0.1:47990",
            "allowed_origins": [],
            "web": {"directory": "__WEB__", "trusted_keys": {"installer-release": "11".repeat(32)}},
        })
    );
    let nvhttp = &policy.settings["amaterasu.nvhttp"];
    assert_eq!(nvhttp["enabled"], true);
    assert_eq!(nvhttp["http_bind"], "127.0.0.1:47989");
    assert_eq!(nvhttp["https_bind"], "127.0.0.1:47984");
    assert_eq!(nvhttp["allow_remote"], false);
    assert!(nvhttp.get("streaming").is_none());
    assert_eq!(
        policy.settings["amaterasu.pairing"]["transport_principals"],
        json!(["amaterasu.nvhttp"])
    );
    assert_eq!(
        policy.settings["amaterasu.authentication"]["administrator_principals"],
        json!(["amaterasu.management"])
    );
    assert_eq!(
        policy.settings["amaterasu.catalog"]["export_principals"],
        json!({"amaterasu.nvhttp": "amaterasu.management"})
    );
    for component in [
        "amaterasu.capture-dxgi",
        "amaterasu.capture-wgc",
        "amaterasu.audio",
        "amaterasu.audio-encode",
        "amaterasu.encode",
        "amaterasu.video",
        "amaterasu.media-transport",
        "amaterasu.control-transport",
        "amaterasu.stream-session",
        "amaterasu.rtsp",
        "amaterasu.virtual-display",
    ] {
        assert_eq!(policy.settings[component]["enabled"], false, "{component}");
    }
}

#[test]
fn public_launch_packages_preserve_transitive_delegation_and_identity_only_native_access() {
    for profile in [PackageProfile::Platform, PackageProfile::StreamProbe] {
        let adapter = profile
            .components()
            .iter()
            .find(|component| component.id == NVHTTP.id)
            .unwrap();
        assert!(adapter.dependencies.contains(&LAUNCH.id));
        assert!(adapter.dependencies.contains(&RTSP.id));
        assert!(adapter.calls.contains(&"amaterasu.launch/1"));
        assert!(adapter.calls.contains(&"amaterasu.rtsp/1"));
        for downstream in [LAUNCH, RTSP] {
            assert!(
                downstream
                    .calls
                    .iter()
                    .all(|service| adapter.calls.contains(service))
            );
        }
        assert_eq!(adapter.native_uses, &["amaterasu.identity/1"]);
    }
}
