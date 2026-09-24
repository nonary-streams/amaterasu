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
fn management_loads_after_its_nvhttp_settings_provider() {
    assert!(MANAGEMENT.calls.contains(&"amaterasu.nvhttp/1"));
    assert!(MANAGEMENT.dependencies.contains(&NVHTTP.id));
    for service in [
        "amaterasu.rtsp/1",
        "amaterasu.media-transport/1",
        "amaterasu.control-transport/1",
    ] {
        assert!(
            MANAGEMENT.calls.contains(&service),
            "admin NVHTTP port updates must delegate {service}"
        );
    }
}

#[test]
fn launch_can_call_only_the_typed_playnite_provider_service_it_uses() {
    assert!(LAUNCH.calls.contains(&"amaterasu.playnite/1"));
    assert!(LAUNCH.dependencies.contains(&PLAYNITE.id));
    assert!(!PLAYNITE.calls.contains(&"amaterasu.launch/1"));
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
                .filter(|service| **service != "amaterasu.session-history/1")
                .all(|service| rtsp.calls.contains(service))
        );
        // History is internal to stream-session; RTSP has no direct grant.
        assert!(
            STREAM_SESSION
                .calls
                .contains(&"amaterasu.session-history/1")
        );
        assert!(!rtsp.calls.contains(&"amaterasu.session-history/1"));
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
    let history_database = std::path::PathBuf::from(
        policy.settings["amaterasu.session-history"]["database_path"]
            .as_str()
            .unwrap(),
    );
    assert!(history_database.is_absolute());
    let private_state_directory = root.join("data").join("state").canonicalize().unwrap();
    assert!(history_database.starts_with(private_state_directory));
    assert_eq!(history_database.file_name().unwrap(), "history.db");
    let http_settings = &policy.settings[NVHTTP.id];
    assert_eq!(http_settings["enabled"], false);
    assert_eq!(http_settings["address_family"], "ipv4");
    assert_eq!(http_settings["bind_address"], "");
    assert!(http_settings.get("streaming").is_none());
    let settings = &policy.settings[RTSP.id];
    assert_eq!(settings["enabled"], false);
    assert_eq!(settings["allow_remote"], false);
    assert_eq!(settings["address_family"], "ipv4");
    assert_eq!(settings["bind_address"], "");
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
    assert_eq!(nvhttp["address_family"], "ipv4");
    assert_eq!(nvhttp["bind_address"], "");
    assert!(nvhttp.get("streaming").is_none());
    let rtsp = &policy.settings[RTSP.id];
    assert_eq!(rtsp["address_family"], "ipv4");
    assert_eq!(rtsp["bind_address"], "");
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
fn authentication_token_ttl_defaults_and_configuration_grant_survive_policy_reload() {
    assert!(AUTHENTICATION.calls.contains(&"amaterasu.configuration/1"));
    assert!(AUTHENTICATION.dependencies.contains(&CONFIGURATION.id));
    let root =
        std::env::temp_dir().join(format!("amaterasu-auth-ttl-profile-{}", std::process::id()));
    let development = development_policy(&root).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    for policy in [
        development,
        installer_policy(
            "__WEB__",
            [("installer-release".into(), "11".repeat(32))].into(),
        ),
    ] {
        let settings = &policy.settings["amaterasu.authentication"];
        assert!(
            settings["administrator_principals"]
                .as_array()
                .is_some_and(|principals| principals.contains(&json!("amaterasu.management")))
        );
        assert!(
            policy.principals[AUTHENTICATION.id]
                .call
                .contains("amaterasu.configuration/1")
        );
        assert_eq!(settings["session_token_ttl_seconds"], 7200);
        assert_eq!(settings["remember_me_refresh_token_ttl_seconds"], 604800);

        let reloaded: RuntimeConfig =
            serde_json::from_value(serde_json::to_value(&policy).unwrap()).unwrap();
        assert_eq!(
            reloaded.settings["amaterasu.authentication"]["session_token_ttl_seconds"],
            7200
        );
        assert_eq!(
            reloaded.settings["amaterasu.authentication"]["remember_me_refresh_token_ttl_seconds"],
            604800
        );
    }
}

#[test]
fn input_policy_can_load_durable_settings_and_management_can_edit_them() {
    assert!(INPUT.calls.contains(&"amaterasu.configuration/1"));
    assert!(INPUT.calls.contains(&"amaterasu.state/1"));
    assert!(INPUT.dependencies.contains(&CONFIGURATION.id));
    assert!(INPUT.dependencies.contains(&STATE.id));
    assert!(MANAGEMENT.calls.contains(&"amaterasu.input/1"));
    assert!(MANAGEMENT.dependencies.contains(&INPUT.id));

    let root = std::env::temp_dir().join(format!(
        "amaterasu-input-settings-profile-{}",
        std::process::id()
    ));
    let development = development_policy(&root).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    let installer = installer_policy(
        "__WEB__",
        [("installer-release".into(), "11".repeat(32))].into(),
    );
    for policy in [development, installer] {
        assert!(
            policy.principals[INPUT.id]
                .call
                .contains("amaterasu.configuration/1")
        );
        assert!(
            policy.principals[INPUT.id]
                .call
                .contains("amaterasu.state/1")
        );
        assert!(
            policy.principals[MANAGEMENT.id]
                .call
                .contains("amaterasu.input/1")
        );
    }
}

#[test]
fn provider_imports_have_fixed_catalog_owners_and_narrow_service_grants() {
    assert!(STEAM.calls.contains(&"amaterasu.catalog/1"));
    assert!(STEAM.dependencies.contains(&CATALOG.id));
    assert!(PLAYNITE.calls.contains(&"amaterasu.catalog/1"));
    assert!(PLAYNITE.calls.contains(&"amaterasu.configuration/1"));
    assert!(PLAYNITE.dependencies.contains(&CATALOG.id));
    assert!(LUTRIS.calls.contains(&"amaterasu.catalog/1"));
    assert!(LUTRIS.calls.contains(&"amaterasu.configuration/1"));
    assert!(LUTRIS.calls.contains(&"amaterasu.steam/1"));
    assert!(LUTRIS.dependencies.contains(&CATALOG.id));
    assert!(LUTRIS.dependencies.contains(&STEAM.id));
    assert!(LUTRIS.dependencies.contains(&STATE.id));
    assert!(PLAYNITE.dependencies.contains(&STATE.id));

    let root = std::env::temp_dir().join(format!(
        "amaterasu-steam-import-profile-{}",
        std::process::id()
    ));
    let development = development_policy(&root).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    let installer = installer_policy(
        "__WEB__",
        [("installer-release".into(), "11".repeat(32))].into(),
    );
    for policy in [development, installer] {
        assert_eq!(
            policy.settings[CATALOG.id]["import_principals"],
            json!({
                "amaterasu.steam": "amaterasu.management",
                "amaterasu.playnite": "amaterasu.management",
                "amaterasu.lutris": "amaterasu.management"
            })
        );
        assert!(
            policy.principals[STEAM.id]
                .call
                .contains("amaterasu.catalog/1")
        );
        assert!(
            policy.principals[PLAYNITE.id]
                .call
                .contains("amaterasu.catalog/1")
        );
        assert!(
            policy.principals[LUTRIS.id]
                .call
                .contains("amaterasu.catalog/1")
        );
        assert!(
            policy.principals[LUTRIS.id]
                .call
                .contains("amaterasu.configuration/1")
        );
        assert!(
            policy.principals[LUTRIS.id]
                .call
                .contains("amaterasu.state/1")
        );
        assert!(
            policy.principals[LUTRIS.id]
                .call
                .contains("amaterasu.steam/1")
        );
        assert!(
            policy.principals[MANAGEMENT.id]
                .call
                .contains("amaterasu.lutris/1")
        );
        assert!(
            policy.principals[PLAYNITE.id]
                .call
                .contains("amaterasu.configuration/1")
        );
        assert!(
            policy.principals[PLAYNITE.id]
                .call
                .contains("amaterasu.state/1")
        );
        assert!(
            policy.principals[MANAGEMENT.id]
                .call
                .contains("amaterasu.playnite/1")
        );
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
