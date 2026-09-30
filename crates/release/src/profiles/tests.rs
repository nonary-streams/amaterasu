use super::*;
use amaterasu_contracts::{Command, Response};
use amaterasu_runtime::Broker;

#[test]
fn every_profile_contains_unique_providers_and_all_dependencies() {
    for profile in [
        PackageProfile::Example,
        PackageProfile::Platform,
        PackageProfile::MediaProbe,
        PackageProfile::StreamProbe,
        PackageProfile::SoftwareProbe,
        PackageProfile::LinuxProbe,
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
fn software_probe_uses_one_real_video_encoder_with_controlled_capture_and_audio() {
    let components = PackageProfile::SoftwareProbe.components();
    let component = |id| {
        components
            .iter()
            .find(|component| component.id == id)
            .unwrap()
    };
    assert_eq!(component(VIDEO.id).library, VIDEO.library);
    assert_eq!(component(ENCODE.id).library, ENCODE.library);
    assert_eq!(
        component(CAPTURE_SOURCE.id).library,
        "amaterasu_capture_source"
    );
    assert_eq!(
        component(SOFTWARE_AUDIO_SOURCE.id).library,
        STREAM_SOURCE.library
    );
    assert_eq!(
        SOFTWARE_AUDIO_SOURCE.provides,
        &["amaterasu.audio/1", "test.stream-source/1"]
    );
    assert_eq!(
        SOFTWARE_AUDIO_SOURCE.native_provides,
        &["amaterasu.audio/1"]
    );
    assert!(SOFTWARE_VIDEO.dependencies.contains(&CAPTURE_SOURCE.id));
    assert!(
        STREAM_SESSION_SOFTWARE_PROBE
            .dependencies
            .contains(&VIDEO.id)
    );
    assert!(
        STREAM_SESSION_SOFTWARE_PROBE
            .dependencies
            .contains(&SOFTWARE_AUDIO_SOURCE.id)
    );
    for production_capture in [CAPTURE_DXGI.id, CAPTURE_WGC.id, AUDIO.id, INPUT.id] {
        assert!(
            !components
                .iter()
                .any(|component| component.id == production_capture)
        );
    }
    let platform = PackageProfile::Platform.components();
    assert!(
        platform
            .iter()
            .any(|component| component.id == CAPTURE_DXGI.id)
    );
    assert!(
        !platform
            .iter()
            .any(|component| component.id == CAPTURE_SOURCE.id)
    );
}

#[test]
fn software_probe_does_not_narrow_existing_development_native_grants() {
    let directory = std::env::temp_dir().join(format!(
        "amaterasu-software-probe-profile-{}",
        std::process::id()
    ));
    let policy = development_policy(&directory).unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert_eq!(
        policy.principals[VIDEO.id].native_use,
        VIDEO.grants().native_use
    );
    assert_eq!(
        policy.principals[STREAM_SOURCE.id].native_provide,
        STREAM_SOURCE.grants().native_provide
    );
    assert_eq!(
        policy.principals[CAPTURE_SOURCE.id].native_provide,
        CAPTURE_SOURCE.grants().native_provide
    );
    let installer = installer_policy("web", BTreeMap::new());
    assert!(!installer.principals.contains_key(CAPTURE_SOURCE.id));
    assert!(!installer.principals.contains_key(SOFTWARE_AUDIO_SOURCE.id));
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
fn nvhttp_upnp_management_poll_has_a_scoped_call_grant_without_a_load_cycle() {
    let service = "amaterasu.management/1";
    assert!(NVHTTP.calls.contains(&service));
    assert!(!NVHTTP.dependencies.contains(&MANAGEMENT.id));
    assert!(MANAGEMENT.dependencies.contains(&NVHTTP.id));

    let components = PackageProfile::Platform.components();
    let nvhttp = components
        .iter()
        .find(|component| component.id == NVHTTP.id)
        .unwrap();
    assert!(nvhttp.calls.contains(&service));
    assert!(!nvhttp.dependencies.contains(&MANAGEMENT.id));

    let directory = std::env::temp_dir().join(format!(
        "amaterasu-nvhttp-upnp-profile-{}",
        std::process::id()
    ));
    let policy = development_policy(&directory).unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert_eq!(policy.principals[NVHTTP.id].call, NVHTTP.grants().call);
    let management_calls = &policy.principals[MANAGEMENT.id].call;
    assert!(
        MANAGEMENT
            .calls
            .iter()
            .all(|service| management_calls.contains(*service))
    );
    assert!(management_calls.contains("amaterasu.capture-pipewire/1"));
}

#[test]
fn stream_probe_uses_real_management_authority_with_synthetic_dependencies() {
    let components = PackageProfile::StreamProbe.components();
    let management = components
        .iter()
        .find(|component| component.id == MANAGEMENT.id)
        .unwrap();
    assert_eq!(management.library, MANAGEMENT.library);
    assert_eq!(management.calls, MANAGEMENT.calls);
    assert_eq!(
        serde_json::to_value(management.grants()).unwrap(),
        serde_json::to_value(MANAGEMENT.grants()).unwrap()
    );
    for dependency in [
        AUTHENTICATION.id,
        HOST_CONFIGURATION.id,
        DIAGNOSTICS.id,
        INPUT_PROBE.id,
        STREAM_SOURCE.id,
        NVHTTP.id,
    ] {
        assert!(management.dependencies.contains(&dependency));
        assert!(
            components
                .iter()
                .any(|component| component.id == dependency)
        );
    }
    for dependency in [
        INPUT.id,
        VIDEO.id,
        AUDIO.id,
        CAPTURE_DXGI.id,
        CAPTURE_WGC.id,
        ENCODE.id,
    ] {
        assert!(!management.dependencies.contains(&dependency));
    }
}

#[test]
fn signed_platform_grants_preserve_display_and_launch_parent_authority() {
    let policy = installer_policy("__WEB__", BTreeMap::new());
    let display = "amaterasu.display/1";
    let state = "amaterasu.state/1";
    let launch = "amaterasu.launch/1";
    let chain = [NVHTTP, RTSP, STREAM_SESSION];

    for component in chain {
        assert!(
            policy.principals[component.id].call.contains(display),
            "{} cannot delegate display Apply/Status",
            component.id
        );
        assert!(component.dependencies.contains(&DISPLAY.id));
        assert!(
            policy.principals[component.id].call.contains(state),
            "{} cannot delegate display journal state",
            component.id
        );
        assert!(component.dependencies.contains(&STATE.id));
    }
    for component in [NVHTTP, RTSP] {
        assert!(policy.principals[component.id].call.contains(launch));
        assert!(component.dependencies.contains(&LAUNCH.id));
    }
    assert!(policy.principals[DISPLAY.id].call.contains(state));
    assert_eq!(policy.settings[DISPLAY.id]["allow_mutation"], false);
    for component in [PLAYNITE, LUTRIS, AUTHENTICATION, PAIRING, INPUT] {
        assert!(!policy.principals[component.id].call.contains(display));
        assert!(!policy.principals[component.id].call.contains(launch));
    }

    let probe = PackageProfile::StreamProbe.components();
    assert!(probe.iter().any(|component| component.id == DISPLAY.id));
    assert!(STREAM_SESSION_PROBE.dependencies.contains(&DISPLAY.id));
    assert!(STREAM_SESSION_PROBE.dependencies.contains(&STATE.id));
}

#[test]
fn signed_platform_call_intersections_cover_active_and_cleanup_display_paths() {
    fn delegates(policy: &RuntimeConfig, origin: &str, calls: &[(&str, &str)]) -> bool {
        let Some(origin_grants) = policy.principals.get(origin) else {
            return false;
        };
        let mut allowed_services = origin_grants.call.clone();
        for (caller, service) in calls {
            let Some(grants) = policy.principals.get(*caller) else {
                return false;
            };
            if !allowed_services.contains(*service) || !grants.call.contains(*service) {
                return false;
            }
            allowed_services = allowed_services
                .intersection(&grants.call)
                .cloned()
                .collect();
        }
        true
    }

    let policy = installer_policy("__WEB__", BTreeMap::new());
    let active_display = [
        (NVHTTP.id, "amaterasu.rtsp/1"),
        (RTSP.id, "amaterasu.stream-session/1"),
        (STREAM_SESSION.id, "amaterasu.display/1"),
        (DISPLAY.id, "amaterasu.state/1"),
    ];
    assert!(delegates(&policy, NVHTTP.id, &active_display));
    assert!(delegates(
        &policy,
        STREAM_SESSION.id,
        &[
            (STREAM_SESSION.id, "amaterasu.display/1"),
            (DISPLAY.id, "amaterasu.state/1"),
        ],
    ));
    assert!(delegates(
        &policy,
        NVHTTP.id,
        &[
            (NVHTTP.id, "amaterasu.rtsp/1"),
            (RTSP.id, "amaterasu.launch/1"),
        ],
    ));

    for unrelated in [PLAYNITE.id, LUTRIS.id, AUTHENTICATION.id, INPUT.id] {
        assert!(!delegates(
            &policy,
            unrelated,
            &[(unrelated, "amaterasu.display/1")],
        ));
    }
    for ancestor in [NVHTTP.id, RTSP.id, STREAM_SESSION.id] {
        let mut narrowed = policy.clone();
        narrowed
            .principals
            .get_mut(ancestor)
            .unwrap()
            .call
            .remove("amaterasu.display/1");
        assert!(!delegates(&narrowed, NVHTTP.id, &active_display));
    }
}

#[test]
fn installer_policy_authorizes_real_parented_display_and_launch_requests() {
    fn accepted(response: Response) -> u64 {
        match response {
            Response::Accepted { id } => id,
            other => panic!("broker rejected expected call: {other:?}"),
        }
    }

    fn send(
        broker: &mut Broker,
        caller: &str,
        service: &str,
        operation_id: &str,
        parent: Option<u64>,
        action: &str,
    ) -> u64 {
        accepted(broker.dispatch(
            caller,
            Command::Request {
                service: service.into(),
                payload: json!({"action": action}),
                timeout_ms: 30_000,
                operation_id: operation_id.into(),
                parent,
            },
        ))
    }

    fn delivered(broker: &mut Broker, provider: &str, id: u64, initiator: &str) {
        assert!(
            matches!(broker.dispatch(provider, Command::Poll {}),
                Response::Request { id: delivered, initiator: actual, .. }
                    if delivered == id && actual == initiator),
            "{provider} did not receive parented request {id}"
        );
    }

    let mut broker = Broker::new(installer_policy("__WEB__", BTreeMap::new())).unwrap();
    for component in [RTSP, STREAM_SESSION, DISPLAY, STATE, LAUNCH] {
        assert!(matches!(
            broker.dispatch(
                component.id,
                Command::Register {
                    service: component.provides[0].into(),
                },
            ),
            Response::Ok
        ));
    }

    let rtsp = send(
        &mut broker,
        NVHTTP.id,
        "amaterasu.rtsp/1",
        "rtsp-active",
        None,
        "open",
    );
    delivered(&mut broker, RTSP.id, rtsp, NVHTTP.id);
    let session = send(
        &mut broker,
        RTSP.id,
        "amaterasu.stream-session/1",
        "session-active",
        Some(rtsp),
        "start",
    );
    delivered(&mut broker, STREAM_SESSION.id, session, NVHTTP.id);
    for (operation, action) in [("display-apply", "apply"), ("display-status", "status")] {
        let display = send(
            &mut broker,
            STREAM_SESSION.id,
            "amaterasu.display/1",
            operation,
            Some(session),
            action,
        );
        delivered(&mut broker, DISPLAY.id, display, NVHTTP.id);
        let state = send(
            &mut broker,
            DISPLAY.id,
            "amaterasu.state/1",
            &format!("{operation}-journal"),
            Some(display),
            "get",
        );
        delivered(&mut broker, STATE.id, state, NVHTTP.id);
    }

    for (operation, action) in [("cleanup-status", "status"), ("cleanup-restore", "restore")] {
        let display = send(
            &mut broker,
            STREAM_SESSION.id,
            "amaterasu.display/1",
            operation,
            None,
            action,
        );
        delivered(&mut broker, DISPLAY.id, display, STREAM_SESSION.id);
        let state = send(
            &mut broker,
            DISPLAY.id,
            "amaterasu.state/1",
            &format!("{operation}-journal"),
            Some(display),
            "get",
        );
        delivered(&mut broker, STATE.id, state, STREAM_SESSION.id);
    }

    let launch = send(
        &mut broker,
        RTSP.id,
        "amaterasu.launch/1",
        "launch-verify",
        Some(rtsp),
        "verify",
    );
    delivered(&mut broker, LAUNCH.id, launch, NVHTTP.id);

    for outsider in [PLAYNITE.id, LUTRIS.id, AUTHENTICATION.id, INPUT.id] {
        assert!(matches!(
            broker.dispatch(
                outsider,
                Command::Request {
                    service: "amaterasu.display/1".into(),
                    payload: json!({"action":"apply"}),
                    timeout_ms: 30_000,
                    operation_id: "unauthorized-display".into(),
                    parent: None,
                },
            ),
            Response::Error { code, .. } if code == "permission_denied"
        ));
    }

    let mut narrowed_policy = installer_policy("__WEB__", BTreeMap::new());
    narrowed_policy
        .principals
        .get_mut(NVHTTP.id)
        .unwrap()
        .call
        .remove("amaterasu.display/1");
    let mut narrowed = Broker::new(narrowed_policy).unwrap();
    for component in [RTSP, STREAM_SESSION, DISPLAY] {
        assert!(matches!(
            narrowed.dispatch(
                component.id,
                Command::Register {
                    service: component.provides[0].into(),
                },
            ),
            Response::Ok
        ));
    }
    let rtsp = send(
        &mut narrowed,
        NVHTTP.id,
        "amaterasu.rtsp/1",
        "rtsp-narrow",
        None,
        "open",
    );
    delivered(&mut narrowed, RTSP.id, rtsp, NVHTTP.id);
    let session = send(
        &mut narrowed,
        RTSP.id,
        "amaterasu.stream-session/1",
        "session-narrow",
        Some(rtsp),
        "start",
    );
    delivered(&mut narrowed, STREAM_SESSION.id, session, NVHTTP.id);
    assert!(matches!(
        narrowed.dispatch(
            STREAM_SESSION.id,
            Command::Request {
                service: "amaterasu.display/1".into(),
                payload: json!({"action":"apply"}),
                timeout_ms: 30_000,
                operation_id: "display-narrow".into(),
                parent: Some(session),
            },
        ),
        Response::Error { code, .. } if code == "permission_denied"
    ));
}

#[test]
fn platform_registers_host_configuration_with_runtime_and_state_dependencies() {
    let platform = PackageProfile::Platform.components();
    let provider = platform
        .iter()
        .find(|component| component.id == HOST_CONFIGURATION.id)
        .unwrap();

    assert_eq!(provider.library, "amaterasu_host_configuration");
    assert_eq!(provider.provides, &["amaterasu.host-configuration/1"]);
    assert_eq!(
        provider.calls,
        &["amaterasu.state/1", "amaterasu.diagnostics/1"]
    );
    assert!(provider.dependencies.contains(&RUNTIME_ID));
    assert!(provider.dependencies.contains(&STATE.id));
    let pairing = platform
        .iter()
        .find(|component| component.id == PLATFORM_PAIRING.id)
        .unwrap();
    assert_eq!(
        pairing.calls,
        &["amaterasu.state/1", "amaterasu.host-configuration/1"]
    );
    assert_eq!(pairing.calls.len(), 2);
    assert_eq!(pairing.dependencies.len(), 3);
    assert!(pairing.dependencies.contains(&RUNTIME_ID));
    assert!(pairing.dependencies.contains(&HOST_CONFIGURATION.id));
    assert!(pairing.dependencies.contains(&STATE.id));
    let provider_index = platform
        .iter()
        .position(|component| component.id == HOST_CONFIGURATION.id)
        .unwrap();
    let pairing_index = platform
        .iter()
        .position(|component| component.id == pairing.id)
        .unwrap();
    assert!(provider_index < pairing_index);
    assert!(
        !PackageProfile::Example
            .components()
            .iter()
            .any(|component| component.id == HOST_CONFIGURATION.id)
    );
    assert!(
        !PackageProfile::MediaProbe
            .components()
            .iter()
            .any(|component| component.id == HOST_CONFIGURATION.id)
    );
    let stream_probe = PackageProfile::StreamProbe.components();
    assert!(
        stream_probe
            .iter()
            .any(|component| component.id == HOST_CONFIGURATION.id)
    );
    let legacy_pairing = stream_probe
        .iter()
        .find(|component| component.id == PAIRING.id)
        .unwrap();
    assert_eq!(legacy_pairing.calls, PAIRING.calls);
    assert_eq!(legacy_pairing.dependencies, PAIRING.dependencies);
}

#[test]
fn diagnostics_and_management_receive_narrow_host_configuration_grants() {
    let host_configuration_service = "amaterasu.host-configuration/1";
    let state_service = "amaterasu.state/1";
    let diagnostics_service = "amaterasu.diagnostics/1";
    assert_eq!(
        DIAGNOSTICS.calls,
        &[host_configuration_service, state_service]
    );
    assert!(DIAGNOSTICS.dependencies.contains(&HOST_CONFIGURATION.id));
    assert!(MANAGEMENT.calls.contains(&host_configuration_service));
    assert!(MANAGEMENT.dependencies.contains(&HOST_CONFIGURATION.id));

    let root = std::env::temp_dir().join(format!(
        "amaterasu-host-configuration-profile-{}",
        std::process::id()
    ));
    let development = development_policy(&root).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    let installer = installer_policy(
        "__WEB__",
        [("installer-release".into(), "11".repeat(32))].into(),
    );

    for policy in [development, installer] {
        let diagnostics_calls = &policy.principals[DIAGNOSTICS.id].call;
        assert_eq!(
            diagnostics_calls,
            &names(&[host_configuration_service, state_service])
        );
        assert!(
            policy.principals[MANAGEMENT.id]
                .call
                .contains(host_configuration_service)
        );

        let host_configuration = &policy.principals[HOST_CONFIGURATION.id];
        assert_eq!(host_configuration.provide.len(), 1);
        assert!(
            host_configuration
                .provide
                .contains(host_configuration_service)
        );
        assert_eq!(host_configuration.call.len(), 2);
        assert!(host_configuration.call.contains(state_service));
        assert!(host_configuration.call.contains(diagnostics_service));
        assert!(!host_configuration.call.contains(host_configuration_service));
        assert!(
            !policy.principals[PAIRING.id]
                .call
                .contains(diagnostics_service)
        );
        assert_eq!(policy.settings[PAIRING.id]["host_configuration"], true);
        assert_eq!(
            policy.principals[PAIRING.id].call.len(),
            2,
            "configured pairing may call state and read-only host configuration"
        );
        assert!(
            policy.principals[PAIRING.id]
                .call
                .contains("amaterasu.state/1")
        );
        assert!(
            policy.principals[PAIRING.id]
                .call
                .contains("amaterasu.host-configuration/1")
        );
    }
}

#[test]
fn signed_policy_delivers_parented_host_logging_apply_to_diagnostics() {
    let mut broker = Broker::new(installer_policy("__WEB__", BTreeMap::new())).unwrap();
    for component in [HOST_CONFIGURATION, DIAGNOSTICS] {
        assert!(matches!(
            broker.dispatch(
                component.id,
                Command::Register {
                    service: component.provides[0].into(),
                },
            ),
            Response::Ok
        ));
    }

    let parent = broker.dispatch(
        MANAGEMENT.id,
        Command::Request {
            service: "amaterasu.host-configuration/1".into(),
            payload: json!({"action":"update"}),
            timeout_ms: 30_000,
            operation_id: "host-logging-update".into(),
            parent: None,
        },
    );
    let Response::Accepted { id: parent_id } = parent else {
        panic!("management host configuration update was rejected: {parent:?}");
    };
    assert!(matches!(
        broker.dispatch(HOST_CONFIGURATION.id, Command::Poll {}),
        Response::Request { id, initiator, .. }
            if id == parent_id && initiator == MANAGEMENT.id
    ));

    let child = broker.dispatch(
        HOST_CONFIGURATION.id,
        Command::Request {
            service: "amaterasu.diagnostics/1".into(),
            payload: json!({"action":"apply_host_logging"}),
            timeout_ms: 30_000,
            operation_id: "host-logging-apply".into(),
            parent: Some(parent_id),
        },
    );
    let Response::Accepted { id: child_id } = child else {
        panic!("parented diagnostics apply was rejected: {child:?}");
    };
    assert!(matches!(
        broker.dispatch(DIAGNOSTICS.id, Command::Poll {}),
        Response::Request { id, initiator, .. }
            if id == child_id && initiator == MANAGEMENT.id
    ));

    assert!(matches!(
        broker.dispatch(
            PAIRING.id,
            Command::Request {
                service: "amaterasu.diagnostics/1".into(),
                payload: json!({"action":"apply_host_logging"}),
                timeout_ms: 30_000,
                operation_id: "unauthorized-logging-apply".into(),
                parent: None,
            },
        ),
        Response::Error { code, .. } if code == "permission_denied"
    ));
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
                .filter(|service| {
                    !["amaterasu.session-history/1", "amaterasu.frame-limiter/1"].contains(service)
                })
                .all(|service| rtsp.calls.contains(service))
        );
        // History and frame limiter are internal to stream-session; RTSP has no direct grant.
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
fn stream_profiles_register_frame_limiter_with_only_stream_session_call_grant() {
    let service = "amaterasu.frame-limiter/1";
    assert_eq!(FRAME_LIMITER.library, "amaterasu_frame_limiter");
    assert_eq!(FRAME_LIMITER.provides, &[service]);
    assert_eq!(FRAME_LIMITER.dependencies, &[RUNTIME_ID]);
    assert!(FRAME_LIMITER.calls.is_empty());

    for profile in [PackageProfile::Platform, PackageProfile::StreamProbe] {
        let components = profile.components();
        assert!(
            components
                .iter()
                .any(|component| component.id == FRAME_LIMITER.id)
        );
        let session = components
            .iter()
            .find(|component| component.id == STREAM_SESSION.id)
            .unwrap();
        assert!(session.dependencies.contains(&FRAME_LIMITER.id));
        assert!(session.calls.contains(&service));
        for component in components
            .iter()
            .filter(|component| component.id != STREAM_SESSION.id)
        {
            assert!(
                !component.calls.contains(&service),
                "{} has frame limiter grant",
                component.id
            );
        }
    }

    let root = std::env::temp_dir().join(format!(
        "amaterasu-frame-limiter-profile-{}",
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
            policy.principals[FRAME_LIMITER.id].provide,
            names(&[service])
        );
        assert!(policy.principals[STREAM_SESSION.id].call.contains(service));
        for (principal, grants) in &policy.principals {
            if principal != STREAM_SESSION.id && principal != "amaterasu.console" {
                assert!(
                    !grants.call.contains(service),
                    "{principal} has frame limiter grant"
                );
            }
        }
    }
}

#[test]
fn session_history_loads_its_policy_from_central_configuration() {
    assert!(SESSION_HISTORY.calls.contains(&"amaterasu.configuration/1"));
    assert!(SESSION_HISTORY.calls.contains(&"amaterasu.state/1"));
    assert!(SESSION_HISTORY.dependencies.contains(&CONFIGURATION.id));
    for profile in [PackageProfile::Platform, PackageProfile::StreamProbe] {
        let components = profile.components();
        assert!(
            components
                .iter()
                .any(|component| component.id == SESSION_HISTORY.id)
        );
        assert!(
            components
                .iter()
                .any(|component| component.id == CONFIGURATION.id)
        );
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
fn catalog_profiles_scope_lutris_steam_id_export_authorization() {
    let root = std::env::temp_dir().join(format!(
        "amaterasu-catalog-export-profile-{}",
        std::process::id()
    ));
    let development = development_policy(&root).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    let installer = installer_policy(
        "__WEB__",
        [("installer-release".into(), "11".repeat(32))].into(),
    );

    for policy in [development, installer] {
        let catalog = &policy.settings[CATALOG.id];
        assert_eq!(
            catalog["steam_id_export_principals"],
            json!({"amaterasu.lutris": "amaterasu.management"})
        );
        assert_eq!(
            catalog["export_principals"],
            json!({"amaterasu.nvhttp": "amaterasu.management"})
        );
        assert!(
            policy.settings[AUTHENTICATION.id]["allowed_scopes"]
                .as_array()
                .is_some_and(|scopes| scopes.contains(&json!("lutris.read")))
        );
        for (principal, grants) in &policy.principals {
            if principal != "amaterasu.console" {
                assert_eq!(
                    grants.call.contains("amaterasu.lutris/1"),
                    principal == MANAGEMENT.id || principal == LAUNCH.id || principal == NVHTTP.id,
                    "unexpected Lutris call grant for {principal}"
                );
            }
        }
    }
}

#[test]
fn generated_policies_grant_launch_only_the_expected_provider_calls() {
    let root = std::env::temp_dir().join(format!(
        "amaterasu-lutris-launch-grants-{}",
        std::process::id()
    ));
    let development = development_policy(&root).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    let installer = installer_policy("__WEB__", BTreeMap::new());
    let expected_calls = names(&[
        "amaterasu.catalog/1",
        "amaterasu.state/1",
        "amaterasu.session/1",
        "amaterasu.application/1",
        "amaterasu.playnite/1",
        "amaterasu.lutris/1",
    ]);
    let expected_dependencies = [
        RUNTIME_ID,
        CATALOG.id,
        STATE.id,
        SESSION.id,
        APPLICATION.id,
        PLAYNITE.id,
        LUTRIS.id,
    ];

    assert_eq!(LAUNCH.dependencies, expected_dependencies);
    for policy in [development, installer] {
        assert_eq!(policy.principals[LAUNCH.id].call, expected_calls);
        assert!(policy.principals.contains_key(LUTRIS.id));
        assert!(
            policy.principals[LUTRIS.id]
                .provide
                .contains("amaterasu.lutris/1")
        );
        assert!(
            policy.principals[NVHTTP.id]
                .call
                .contains("amaterasu.lutris/1")
        );
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
fn authorization_policy_generations_are_distinct_nonzero_and_survive_reload() {
    let root = std::env::temp_dir().join(format!(
        "amaterasu-auth-generation-profile-{}",
        std::process::id()
    ));
    let development = development_policy(&root).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    let installer = installer_policy(
        "__WEB__",
        [("installer-release".into(), "11".repeat(32))].into(),
    );

    let development_generation =
        development.settings["amaterasu.authentication"]["authorization_policy_generation"]
            .as_u64()
            .unwrap();
    let installer_generation =
        installer.settings["amaterasu.authentication"]["authorization_policy_generation"]
            .as_u64()
            .unwrap();
    assert_eq!(development_generation, 4);
    assert_eq!(installer_generation, 3);
    assert_ne!(development_generation, 0);
    assert_ne!(installer_generation, 0);
    assert_ne!(development_generation, installer_generation);

    for (policy, expected_generation) in [(development, 4), (installer, 3)] {
        let serialized = serde_json::to_value(&policy).unwrap();
        assert_eq!(
            serialized["settings"]["amaterasu.authentication"]["authorization_policy_generation"],
            expected_generation
        );

        let reloaded: RuntimeConfig = serde_json::from_value(serialized).unwrap();
        assert_eq!(
            reloaded.settings["amaterasu.authentication"]["authorization_policy_generation"],
            expected_generation
        );
    }
}

#[test]
fn installer_authentication_upgrade_changes_only_generation_and_lutris_scope() {
    // This is the authentication entry generated by the previous installer.
    // Existing installations may still have it as their bootstrap policy.
    let previous_installer_authentication = json!({
        "administrator_principals": ["amaterasu.management"],
        "authorization_policy_generation": 2,
        "session_token_ttl_seconds": 7200,
        "remember_me_refresh_token_ttl_seconds": 604800,
        "allowed_scopes": [
            "catalog.read", "catalog.write", "configuration.read", "configuration.write",
            "session.read", "session.write", "application.read", "application.launch",
            "application.terminate", "steam.read", "steam.refresh", "diagnostics.read",
            "diagnostics.write", "pairing.admin", "display.read", "display.write", "media.read"
        ],
    });
    let mut upgraded_authentication = previous_installer_authentication.clone();
    upgraded_authentication["authorization_policy_generation"] = json!(3);
    let scopes = upgraded_authentication["allowed_scopes"]
        .as_array_mut()
        .unwrap();
    let diagnostics_index = scopes
        .iter()
        .position(|scope| scope == "diagnostics.read")
        .unwrap();
    scopes.insert(diagnostics_index, json!("lutris.read"));

    let installer = installer_policy(
        "__WEB__",
        [("installer-release".into(), "11".repeat(32))].into(),
    );
    assert_eq!(
        installer.settings[AUTHENTICATION.id], upgraded_authentication,
        "the installed policy upgrade must preserve every other authentication field"
    );

    let root = std::env::temp_dir().join(format!(
        "amaterasu-auth-upgrade-profile-{}",
        std::process::id()
    ));
    let development = development_policy(&root).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    let development_authentication = &development.settings[AUTHENTICATION.id];
    assert_eq!(
        development_authentication["authorization_policy_generation"],
        4
    );
    assert_eq!(
        development_authentication["administrator_principals"],
        json!(["amaterasu.console", "amaterasu.management"])
    );
    let mut development_scopes = upgraded_authentication["allowed_scopes"]
        .as_array()
        .unwrap()
        .clone();
    development_scopes.extend([json!("capture.read"), json!("capture.write")]);
    assert_eq!(
        development_authentication["allowed_scopes"],
        json!(development_scopes)
    );
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

#[test]
fn linux_probe_scopes_portal_capture_to_an_explicit_composition() {
    let components = PackageProfile::LinuxProbe.components();
    assert!(
        components
            .iter()
            .any(|component| component.id == CAPTURE_PIPEWIRE.id)
    );
    assert!(
        components
            .iter()
            .any(|component| component.id == LINUX_VIDEO.id && component.library == VIDEO.library)
    );
    assert!(
        components
            .iter()
            .any(|component| component.id == ENCODE.id && component.library == ENCODE.library)
    );
    assert!(!components.iter().any(|component| {
        [CAPTURE_SOURCE.id, CAPTURE_DXGI.id, CAPTURE_WGC.id, VIDEO.id].contains(&component.id)
    }));
    assert_eq!(
        LINUX_VIDEO.native_uses,
        &["amaterasu.capture-pipewire/1", "amaterasu.encode/1"]
    );
    assert!(LINUX_STREAM_SESSION.dependencies.contains(&LINUX_VIDEO.id));
    for component in [LINUX_NVHTTP, LINUX_RTSP, LINUX_STREAM_SESSION] {
        assert!(!component.calls.contains(&"amaterasu.capture-pipewire/1"));
        assert!(!component.calls.contains(&"amaterasu.capture-dxgi/1"));
        assert!(!component.calls.contains(&"amaterasu.capture-wgc/1"));
    }
    assert!(LINUX_VIDEO.calls.contains(&"amaterasu.capture-pipewire/1"));
    assert!(
        LINUX_MANAGEMENT
            .calls
            .contains(&"amaterasu.capture-pipewire/1")
    );
    assert!(!MANAGEMENT.calls.contains(&"amaterasu.capture-pipewire/1"));
    let root = std::env::temp_dir().join(format!(
        "amaterasu-linux-probe-profile-{}",
        std::process::id()
    ));
    let development = development_policy(&root).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    assert_eq!(development.settings[CAPTURE_PIPEWIRE.id]["enabled"], false);
    assert_eq!(
        development.settings[CAPTURE_PIPEWIRE.id]["selection_principals"],
        json!(["amaterasu.management"])
    );
    assert_eq!(
        development.settings[CAPTURE_PIPEWIRE.id]["capture_principals"],
        json!([])
    );
    assert_eq!(
        development.principals[VIDEO.id].native_use,
        VIDEO.grants().native_use
    );
    assert!(
        development.principals[MANAGEMENT.id]
            .call
            .contains("amaterasu.capture-pipewire/1")
    );
    assert!(
        !development.principals[MANAGEMENT.id]
            .native_use
            .contains("amaterasu.capture-pipewire/1")
    );
    assert!(
        development.settings["amaterasu.authentication"]["allowed_scopes"]
            .as_array()
            .unwrap()
            .contains(&json!("capture.read"))
    );
    assert!(
        development.settings["amaterasu.authentication"]["allowed_scopes"]
            .as_array()
            .unwrap()
            .contains(&json!("capture.write"))
    );
    for component in [NVHTTP, RTSP, STREAM_SESSION] {
        assert!(
            !development.principals[component.id]
                .call
                .contains("amaterasu.capture-pipewire/1")
        );
    }
    let installer = installer_policy("web", BTreeMap::new());
    assert!(!installer.principals.contains_key(CAPTURE_PIPEWIRE.id));
    assert!(!installer.principals.contains_key(LINUX_VIDEO.id));
    assert!(!installer.settings.contains_key(CAPTURE_PIPEWIRE.id));
    for component in PackageProfile::Platform.components() {
        assert!(!component.calls.contains(&"amaterasu.capture-pipewire/1"));
        assert!(
            !component
                .native_uses
                .contains(&"amaterasu.capture-pipewire/1")
        );
    }
}
