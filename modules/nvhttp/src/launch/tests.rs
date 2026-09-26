use super::*;
use amaterasu_module_sdk::{Command, Response};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn config() -> StreamingConfig {
    StreamingConfig {
        capture: CaptureConfig {
            capture_provider: CaptureProvider::Dxgi,
            output: "display-one".into(),
            encoder: 0,
            audio_endpoint: "default".into(),
        },
        profile: StreamProfile {
            max_width: 1920,
            max_height: 1080,
            max_fps: 60,
            bitrate_kbps: 10_000,
            max_reference_frames: 1,
            packet_size: 1024,
            fec_percentage: 20,
            min_parity_shards: 2,
            max_fec_blocks: 1,
            packet_ms: 10,
            audio_bitrate: 128_000,
            encrypt_video: true,
            encrypt_audio: true,
            permit_legacy_audio_iv_wrap: false,
        },
    }
}

fn peer(marker: u8) -> PeerIdentity {
    peer_with_fec(marker, None)
}

fn peer_with_fec(marker: u8, fec_percentage: Option<u8>) -> PeerIdentity {
    peer_with_overrides(marker, fec_percentage, None)
}

fn peer_with_stream_overrides(
    marker: u8,
    max_bitrate: Option<i32>,
    minimum_fps_target: Option<f64>,
    video_max_batch_size_kb: Option<u8>,
) -> PeerIdentity {
    let certificate = vec![marker; 32];
    PeerIdentity::new("127.0.0.1".parse().unwrap(), certificate.clone())
        .unwrap()
        .with_paired_identity(
            format!("client-{marker}"),
            fingerprint(&certificate),
            ClientConfigOverrides {
                max_bitrate,
                minimum_fps_target,
                video_max_batch_size_kb,
                ..Default::default()
            },
        )
        .unwrap()
}

fn peer_with_frame_limiter_overrides(
    marker: u8,
    enable: Option<bool>,
    fps_limit: Option<f64>,
) -> PeerIdentity {
    let certificate = vec![marker; 32];
    PeerIdentity::new("127.0.0.1".parse().unwrap(), certificate.clone())
        .unwrap()
        .with_paired_identity(
            format!("client-{marker}"),
            fingerprint(&certificate),
            ClientConfigOverrides {
                frame_limiter_enable: enable,
                frame_limiter_fps_limit: fps_limit,
                ..Default::default()
            },
        )
        .unwrap()
}

fn peer_with_overrides(
    marker: u8,
    fec_percentage: Option<u8>,
    stream_audio: Option<bool>,
) -> PeerIdentity {
    peer_with_settings(marker, fec_percentage, stream_audio, None)
}

fn peer_with_audio_sink(marker: u8, audio_sink: Option<&str>) -> PeerIdentity {
    peer_with_settings(marker, None, None, audio_sink.map(str::to_owned))
}

fn peer_with_capture_options(
    marker: u8,
    audio_sink: Option<&str>,
    audio_sink_capture_only: Option<bool>,
) -> PeerIdentity {
    let certificate = vec![marker; 32];
    PeerIdentity::new("127.0.0.1".parse().unwrap(), certificate.clone())
        .unwrap()
        .with_paired_identity(
            format!("client-{marker}"),
            fingerprint(&certificate),
            ClientConfigOverrides {
                audio_sink: audio_sink.map(str::to_owned),
                audio_sink_capture_only,
                ..Default::default()
            },
        )
        .unwrap()
}

fn peer_with_virtual_sink(marker: u8, selector: Option<&str>) -> PeerIdentity {
    let certificate = vec![marker; 32];
    PeerIdentity::new("127.0.0.1".parse().unwrap(), certificate.clone())
        .unwrap()
        .with_paired_identity(
            format!("client-{marker}"),
            fingerprint(&certificate),
            ClientConfigOverrides {
                virtual_sink: selector.map(str::to_owned),
                ..Default::default()
            },
        )
        .unwrap()
}

fn opened_rtsp_payload(broker: &Broker) -> Value {
    broker
        .commands
        .iter()
        .find_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => {
                Some(payload.clone())
            }
            _ => None,
        })
        .expect("RTSP open")
}

fn peer_with_settings(
    marker: u8,
    fec_percentage: Option<u8>,
    stream_audio: Option<bool>,
    audio_sink: Option<String>,
) -> PeerIdentity {
    let certificate = vec![marker; 32];
    let certificate_id = fingerprint(&certificate);
    PeerIdentity::new("127.0.0.1".parse().unwrap(), certificate)
        .unwrap()
        .with_paired_identity(
            format!("client-{marker}"),
            certificate_id,
            ClientConfigOverrides {
                fec_percentage,
                stream_audio,
                audio_sink,
                ..Default::default()
            },
        )
        .unwrap()
}

fn peer_with_back_button_timeout(marker: u8, timeout: Option<i32>) -> PeerIdentity {
    let certificate = vec![marker; 32];
    PeerIdentity::new("127.0.0.1".parse().unwrap(), certificate.clone())
        .unwrap()
        .with_paired_identity(
            format!("client-{marker}"),
            fingerprint(&certificate),
            ClientConfigOverrides {
                back_button_timeout: timeout,
                ..Default::default()
            },
        )
        .unwrap()
}

fn peer_with_input_overrides(
    marker: u8,
    controller: Option<bool>,
    gamepad: Option<&str>,
    keyboard: Option<bool>,
    mouse: Option<bool>,
) -> PeerIdentity {
    let certificate = vec![marker; 32];
    PeerIdentity::new("127.0.0.1".parse().unwrap(), certificate.clone())
        .unwrap()
        .with_paired_identity(
            format!("client-{marker}"),
            fingerprint(&certificate),
            ClientConfigOverrides {
                controller,
                gamepad: gamepad.map(str::to_owned),
                keyboard,
                mouse,
                ..Default::default()
            },
        )
        .unwrap()
}

fn peer_with_native_pen_touch(marker: u8, native_pen_touch: Option<bool>) -> PeerIdentity {
    let certificate = vec![marker; 32];
    PeerIdentity::new("127.0.0.1".parse().unwrap(), certificate.clone())
        .unwrap()
        .with_paired_identity(
            format!("client-{marker}"),
            fingerprint(&certificate),
            ClientConfigOverrides {
                native_pen_touch,
                ..Default::default()
            },
        )
        .unwrap()
}

fn request(key: u8) -> LaunchRequest {
    LaunchRequest {
        app_id: 1,
        width: 1280,
        height: 720,
        fps: 60,
        host_audio_enabled: false,
        continuous_audio: false,
        device_name: None,
        remote_input_key: [key; 16],
        key_id: 17,
    }
}

#[test]
fn settings_reject_unsupported_packetization_before_launch() {
    let mut invalid = config();
    invalid.profile.fec_percentage = 0;
    assert_eq!(invalid.validate().unwrap_err(), "invalid_streaming_profile");
}

#[test]
fn pairing_lookup_identity_must_match_the_certificate_bytes() {
    let certificate = vec![7; 32];
    let peer = PeerIdentity::new("127.0.0.1".parse().unwrap(), certificate.clone()).unwrap();
    assert_eq!(
        peer.with_paired_identity(
            "moonlight-id".into(),
            "00".repeat(32),
            ClientConfigOverrides::default(),
        )
        .err()
        .unwrap(),
        "invalid_paired_identity"
    );
    assert_eq!(
        PeerIdentity::new("127.0.0.1".parse().unwrap(), certificate.clone())
            .unwrap()
            .with_paired_identity(
                "moonlight-id".into(),
                fingerprint(&certificate),
                ClientConfigOverrides {
                    audio_sink: Some("bad\nendpoint".into()),
                    ..Default::default()
                },
            )
            .err()
            .unwrap(),
        "invalid_paired_identity"
    );
    let paired = PeerIdentity::new("127.0.0.1".parse().unwrap(), certificate.clone())
        .unwrap()
        .with_paired_identity(
            "moonlight-id".into(),
            fingerprint(&certificate),
            ClientConfigOverrides {
                fec_percentage: Some(37),
                stream_audio: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(paired.paired_client_id(), Some("moonlight-id"));
}

#[test]
fn disabled_streaming_and_foreign_cancel_fail_without_work() {
    let now = Instant::now();
    let mut disabled = Controller::new(None).unwrap();
    assert_eq!(
        disabled.launch(1, request(1), peer(1), now).unwrap().status,
        503
    );

    let mut controller = Controller::new(Some(config())).unwrap();
    assert!(controller.launch(1, request(1), peer(1), now).is_none());
    assert_eq!(controller.cancel(2, &peer(2), now).unwrap().status, 403);
}

#[test]
fn secret_key_reuse_is_global_but_key_id_reuse_is_allowed_for_fresh_sessions() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    assert!(controller.launch(1, request(1), peer(1), now).is_none());

    let mut changed = request(1);
    changed.app_id = 2;
    assert_eq!(
        controller.launch(2, changed, peer(2), now).unwrap().status,
        409
    );
}

#[test]
fn stream_start_waits_for_persisted_output_settings() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_output_name_settings(false, None);

    let reply = controller
        .launch(1, request(1), peer(1), now)
        .expect("unloaded persisted settings reject streaming");
    assert_eq!(reply.status, 503);
    assert!(!controller.busy());
}

#[test]
fn selected_or_default_output_is_snapshotted_into_rtsp_open() {
    for (configured, expected) in [
        (Some("selected-display-id"), "selected-display-id"),
        (None, "display-one"),
    ] {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        controller.set_output_name_settings(true, configured);
        assert!(controller.launch(10, request(1), peer(1), now).is_none());
        // A later settings write affects later sessions, not this launch transaction.
        controller.set_output_name_settings(true, Some("changed-after-launch"));

        let mut broker = Broker::default();
        let _ = poll_until_reply(&mut controller, &mut broker, now);
        let rtsp_open = broker
            .commands
            .iter()
            .find_map(|command| match command {
                Command::Request {
                    service, payload, ..
                } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => Some(payload),
                _ => None,
            })
            .expect("RTSP open request");
        assert_eq!(rtsp_open["capture"]["output"], expected);
    }
}

#[test]
fn configured_device_name_and_moonlight_override_are_snapshotted_separately_from_output() {
    for (override_name, configured, expected) in [
        (None, "Studio PC", "Studio PC"),
        (Some(""), "Studio PC", "Studio PC"),
        (Some("Moonlight Desk"), "Studio PC", "Moonlight Desk"),
    ] {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        controller.set_device_name(configured);
        let mut launch_request = request(1);
        launch_request.device_name = override_name.map(str::to_owned);
        assert!(
            controller
                .launch(10, launch_request, peer(1), now)
                .is_none()
        );

        // The active launch keeps its resolved name after later setting changes.
        controller.set_device_name("Changed after launch");
        let mut broker = Broker::default();
        let _ = poll_until_reply(&mut controller, &mut broker, now);
        let rtsp_open = broker
            .commands
            .iter()
            .find_map(|command| match command {
                Command::Request {
                    service, payload, ..
                } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => Some(payload),
                _ => None,
            })
            .expect("RTSP open request");

        assert_eq!(rtsp_open["device_name"], expected);
        assert_eq!(rtsp_open["capture"]["output"], "display-one");
    }
}

#[test]
fn selected_or_bootstrap_audio_sink_is_snapshotted_into_rtsp_capture_open() {
    for (configured, expected) in [
        (Some("endpoint.selected"), "endpoint.selected"),
        (None, "default"),
    ] {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        controller.set_audio_sink_settings(configured);
        assert!(controller.launch(10, request(1), peer(1), now).is_none());
        controller.set_audio_sink_settings(Some("endpoint.changed"));

        let mut broker = Broker::default();
        let _ = poll_until_reply(&mut controller, &mut broker, now);
        let rtsp_open = broker
            .commands
            .iter()
            .find_map(|command| match command {
                Command::Request {
                    service, payload, ..
                } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => Some(payload),
                _ => None,
            })
            .expect("RTSP open request");
        assert_eq!(rtsp_open["capture"]["audio_endpoint"], expected);
    }
}

#[test]
fn unavailable_selected_audio_sink_fails_without_opening_bootstrap_sink() {
    let now = Instant::now();
    let selected = "endpoint.unavailable";
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_audio_sink_settings(Some(selected));
    let mut broker = Broker {
        unavailable_audio_endpoint: Some(selected.into()),
        ..Broker::default()
    };

    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);

    assert_ne!(reply.status, 200);
    let opened_endpoints: Vec<_> = broker
        .commands
        .iter()
        .filter_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => {
                payload["capture"]["audio_endpoint"].as_str()
            }
            _ => None,
        })
        .collect();
    assert_eq!(opened_endpoints, [selected]);
}

#[test]
fn audio_sink_overrides_preserve_empty_values_and_apply_client_app_global_precedence() {
    for (app, client, expected) in [
        (None, None, "endpoint.global"),
        (Some("endpoint.app"), None, "endpoint.app"),
        (
            Some("endpoint.app"),
            Some("endpoint.client"),
            "endpoint.client",
        ),
        (Some(""), None, "default"),
        (Some("endpoint.app"), Some(""), "default"),
    ] {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        controller.set_audio_sink_settings(Some("endpoint.global"));
        let mut broker = Broker {
            application_audio_sink: app.map(str::to_owned),
            ..Broker::default()
        };
        assert!(
            controller
                .launch(10, request(1), peer_with_audio_sink(1, client), now)
                .is_none()
        );

        let _ = poll_until_reply(&mut controller, &mut broker, now);
        assert_eq!(opened_audio_endpoint(&broker), expected);
    }
}

#[test]
fn audio_sink_capture_only_requires_an_explicit_resolved_sink_and_uses_client_app_global_precedence()
 {
    let cases = [
        (true, None, None, None, None, None, false, "default"),
        (
            true,
            Some("endpoint.global"),
            None,
            None,
            None,
            None,
            true,
            "endpoint.global",
        ),
        (
            true,
            Some("endpoint.global"),
            Some(false),
            None,
            None,
            None,
            false,
            "endpoint.global",
        ),
        (
            true,
            Some("endpoint.global"),
            Some(true),
            Some(false),
            None,
            None,
            false,
            "endpoint.global",
        ),
        (
            false,
            None,
            Some(true),
            None,
            Some("endpoint.app"),
            None,
            true,
            "endpoint.app",
        ),
        (
            true,
            Some("endpoint.global"),
            Some(false),
            Some(true),
            Some("endpoint.app"),
            Some("endpoint.client"),
            true,
            "endpoint.client",
        ),
        (
            true,
            Some("endpoint.global"),
            None,
            None,
            Some(""),
            None,
            false,
            "default",
        ),
    ];

    for (
        index,
        (
            global_enabled,
            global_sink,
            app_enabled,
            client_enabled,
            app_sink,
            client_sink,
            expected,
            endpoint,
        ),
    ) in cases.into_iter().enumerate()
    {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        controller.set_audio_sink_settings(global_sink);
        controller.set_audio_sink_capture_only(global_enabled);
        let peer = peer_with_capture_options(index as u8 + 20, client_sink, client_enabled);
        let mut broker = Broker {
            application_audio_sink: app_sink.map(str::to_owned),
            application_audio_sink_capture_only: app_enabled,
            ..Broker::default()
        };

        assert!(controller.launch(10, request(1), peer, now).is_none());
        controller.set_audio_sink_capture_only(!global_enabled);
        let _ = poll_until_reply(&mut controller, &mut broker, now);
        let rtsp_open = broker
            .commands
            .iter()
            .find_map(|command| match command {
                Command::Request {
                    service, payload, ..
                } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => Some(payload),
                _ => None,
            })
            .expect("RTSP open request");
        assert_eq!(rtsp_open["capture_sink_without_routing"], expected);
        assert_eq!(rtsp_open["capture"]["audio_endpoint"], endpoint);
    }
}

#[test]
fn scoped_back_button_timeout_uses_client_then_application_and_preserves_disable() {
    for (client, application, expected) in [
        (Some(-1), Some(1200), Some(-1)),
        (Some(250), Some(1200), Some(250)),
        (None, Some(1200), Some(1200)),
        (None, None, None),
    ] {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        let peer = peer_with_back_button_timeout(33, client);
        let mut broker = Broker {
            application_back_button_timeout: application,
            ..Broker::default()
        };
        assert!(controller.launch(10, request(1), peer, now).is_none());
        let _ = poll_until_reply(&mut controller, &mut broker, now);
        let rtsp_open = broker
            .commands
            .iter()
            .find_map(|command| match command {
                Command::Request {
                    service, payload, ..
                } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => Some(payload),
                _ => None,
            })
            .expect("RTSP open request");
        assert_eq!(rtsp_open["back_button_timeout"], json!(expected));
    }
}

#[test]
fn scoped_input_overrides_use_client_then_application_precedence_and_preserve_false() {
    let now = Instant::now();
    let client_gamepad = selected_supported_gamepad_profile();
    let client_peer = peer_with_input_overrides(
        42,
        Some(false),
        Some(client_gamepad),
        Some(false),
        Some(true),
    );
    let mut broker = Broker {
        application_input_overrides: Some(json!({
            "controller":true,
            "gamepad":"auto",
            "keyboard":true,
            "mouse":false
        })),
        ..Broker::default()
    };
    let mut controller = Controller::new(Some(config())).unwrap();
    assert!(
        controller
            .launch(10, request(1), client_peer, now)
            .is_none()
    );
    let _ = poll_until_reply(&mut controller, &mut broker, now);

    let rtsp_open = opened_rtsp_payload(&broker);
    assert_eq!(rtsp_open["controller"], false);
    assert_eq!(rtsp_open["gamepad"], client_gamepad);
    assert_eq!(rtsp_open["keyboard"], false);
    assert_eq!(rtsp_open["mouse"], true);

    let application_only = {
        let now = Instant::now();
        let peer = peer(43);
        let mut broker = Broker {
            application_input_overrides: Some(json!({
                "controller":false,
                "gamepad":"auto",
                "keyboard":true,
                "mouse":false
            })),
            ..Broker::default()
        };
        let mut controller = Controller::new(Some(config())).unwrap();
        assert!(controller.launch(11, request(2), peer, now).is_none());
        let _ = poll_until_reply(&mut controller, &mut broker, now);
        opened_rtsp_payload(&broker)
    };
    assert_eq!(application_only["controller"], false);
    assert_eq!(application_only["gamepad"], "auto");
    assert_eq!(application_only["keyboard"], true);
    assert_eq!(application_only["mouse"], false);
}

fn selected_supported_gamepad_profile() -> &'static str {
    #[cfg(windows)]
    {
        "x360"
    }
    #[cfg(target_os = "linux")]
    {
        "xone"
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        "auto"
    }
}

#[test]
fn unavailable_scoped_audio_sink_fails_without_falling_back_to_global() {
    let now = Instant::now();
    let selected = "endpoint.unavailable";
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_audio_sink_settings(Some("endpoint.global"));
    let mut broker = Broker {
        application_audio_sink: Some(selected.into()),
        unavailable_audio_endpoint: Some(selected.into()),
        ..Broker::default()
    };

    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);

    assert_ne!(reply.status, 200);
    assert_eq!(opened_audio_endpoints(&broker), [selected]);
}

#[test]
fn invalid_application_audio_sink_is_rejected_before_rtsp_open() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker {
        application_audio_sink: Some("invalid\nendpoint".into()),
        ..Broker::default()
    };

    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);

    assert_ne!(reply.status, 200);
    assert!(opened_audio_endpoints(&broker).is_empty());
}

#[test]
fn client_audio_sink_override_does_not_leak_to_another_client() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_audio_sink_settings(Some("endpoint.global"));
    let first_peer = peer_with_audio_sink(1, Some("endpoint.client-one"));
    let second_peer = peer_with_audio_sink(2, None);
    let mut broker = Broker::default();

    assert!(
        controller
            .launch(10, request(1), first_peer.clone(), now)
            .is_none()
    );
    let (connection, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(connection, 10);
    assert_eq!(reply.status, 200);
    assert_eq!(opened_audio_endpoint(&broker), "endpoint.client-one");
    controller.reply_delivered(connection, now);

    assert!(controller.cancel(11, &first_peer, now).is_none());
    let (connection, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(connection, 11);
    assert_eq!(reply.status, 200);

    assert!(
        controller
            .launch(12, request(2), second_peer, now)
            .is_none()
    );
    let (connection, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(connection, 12);
    assert_eq!(reply.status, 200);
    assert_eq!(
        opened_audio_endpoints(&broker),
        ["endpoint.client-one", "endpoint.global"]
    );
}

#[test]
fn virtual_sink_overrides_follow_client_app_global_precedence_and_empty_clears_inheritance() {
    fn launch_payload(
        client_selector: Option<&str>,
        application_selector: Option<&str>,
        expected_selector: Option<&str>,
        expected_endpoint: &str,
        expected_capture_only: bool,
    ) -> Value {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        controller.set_audio_sink_settings(Some("endpoint.global"));
        controller.set_audio_sink_capture_only(true);
        controller.set_virtual_sink_settings(Some("virtual.global"));
        let peer = peer_with_virtual_sink(1, client_selector);
        let mut broker = Broker {
            application_virtual_sink: application_selector.map(str::to_owned),
            application_audio_sink: Some("endpoint.application".into()),
            ..Broker::default()
        };

        assert!(controller.launch(10, request(1), peer, now).is_none());
        let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
        assert_eq!(reply.status, 200);
        let payload = opened_rtsp_payload(&broker);
        if let Some(expected_selector) = expected_selector {
            assert_eq!(payload["virtual_sink"], expected_selector);
        } else {
            assert!(payload["virtual_sink"].is_null());
        }
        let expected_channels = expected_selector.map_or(Value::Null, |_| json!(2));
        assert_eq!(payload["channels"], expected_channels);
        assert_eq!(payload["capture"]["audio_endpoint"], expected_endpoint);
        assert_eq!(
            payload["capture_sink_without_routing"],
            expected_capture_only
        );
        payload
    }

    launch_payload(None, None, Some("virtual.global"), "", false);
    launch_payload(
        None,
        Some("virtual.application"),
        Some("virtual.application"),
        "",
        false,
    );
    launch_payload(
        Some("virtual.client"),
        Some("virtual.application"),
        Some("virtual.client"),
        "",
        false,
    );
    // Explicit empty in the client scope clears inherited virtual selectors, then
    // regular endpoint resolution continues from the application preference.
    launch_payload(
        Some(""),
        Some("virtual.application"),
        None,
        "endpoint.application",
        true,
    );
    // Explicit empty in the app scope clears the global selector while the
    // regular application endpoint keeps its capture-only policy.
    launch_payload(None, Some(""), None, "endpoint.application", true);
}

#[test]
fn unavailable_selected_output_fails_without_opening_bootstrap_output() {
    let now = Instant::now();
    let selected = "unavailable-display-id";
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_output_name_settings(true, Some(selected));
    let mut broker = Broker {
        unavailable_output: Some(selected.into()),
        ..Broker::default()
    };

    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);

    assert_ne!(reply.status, 200);
    let opened_outputs: Vec<_> = broker
        .commands
        .iter()
        .filter_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => {
                payload["capture"]["output"].as_str()
            }
            _ => None,
        })
        .collect();
    assert_eq!(opened_outputs, [selected]);
}

struct Broker {
    next: u64,
    results: BTreeMap<u64, Value>,
    commands: Vec<Command>,
    application_state: &'static str,
    fail_rtsp_cleanup: bool,
    wrong_application_id: bool,
    lose_rtsp_open_once: bool,
    lost_rtsp_open: bool,
    application_operation: String,
    application_id: String,
    fail_rtsp_settlement: bool,
    fail_application_settlement: bool,
    unavailable_output: Option<String>,
    unavailable_audio_endpoint: Option<String>,
    unsettled_rtsp: BTreeSet<u64>,
    unsettled_application: BTreeSet<u64>,
    rtsp_closed: bool,
    application_stopped: bool,
    application_unhealthy: bool,
    application_fec_percentage: Option<u8>,
    application_max_bitrate: Option<i32>,
    application_minimum_fps_target: Option<f64>,
    application_video_max_batch_size_kb: Option<u8>,
    application_stream_audio: Option<bool>,
    application_audio_sink: Option<String>,
    application_virtual_sink: Option<String>,
    application_audio_sink_capture_only: Option<bool>,
    application_back_button_timeout: Option<i32>,
    application_input_overrides: Option<Value>,
    application_snapshot: Option<Value>,
    application_executable_path: Option<String>,
    application_catalog_revision: u64,
    rtsp_snapshot: Option<Value>,
    rtsp_executable_path: Option<String>,
    wrong_rtsp_snapshot: bool,
}

impl Default for Broker {
    fn default() -> Self {
        Self {
            next: 0,
            results: BTreeMap::new(),
            commands: Vec::new(),
            application_state: "active",
            fail_rtsp_cleanup: false,
            wrong_application_id: false,
            lose_rtsp_open_once: false,
            lost_rtsp_open: false,
            application_operation: String::new(),
            application_id: "app-0000000000000001".into(),
            fail_rtsp_settlement: false,
            fail_application_settlement: false,
            unavailable_output: None,
            unavailable_audio_endpoint: None,
            unsettled_rtsp: BTreeSet::new(),
            unsettled_application: BTreeSet::new(),
            rtsp_closed: false,
            application_stopped: false,
            application_unhealthy: false,
            application_fec_percentage: None,
            application_max_bitrate: None,
            application_minimum_fps_target: None,
            application_video_max_batch_size_kb: None,
            application_stream_audio: None,
            application_audio_sink: None,
            application_virtual_sink: None,
            application_audio_sink_capture_only: None,
            application_back_button_timeout: None,
            application_input_overrides: None,
            application_snapshot: None,
            application_executable_path: None,
            application_catalog_revision: 1,
            rtsp_snapshot: None,
            rtsp_executable_path: None,
            wrong_rtsp_snapshot: false,
        }
    }
}

impl Broker {
    fn send(&mut self, command: Command) -> Transport {
        self.commands.push(command.clone());
        match command {
            Command::Request {
                service, payload, ..
            } => {
                if self.lose_rtsp_open_once
                    && !self.lost_rtsp_open
                    && service == "amaterasu.rtsp/1"
                    && payload["action"] == "open"
                {
                    self.lost_rtsp_open = true;
                    return Err("lost admission".into());
                }
                self.next += 1;
                let request = self.next;
                if service == "amaterasu.rtsp/1" && payload["action"] == "close" {
                    self.rtsp_closed = true;
                    self.unsettled_rtsp.insert(request);
                }
                if service == "amaterasu.launch/1" && payload["action"] == "stop" {
                    self.application_stopped = true;
                    self.unsettled_application.insert(request);
                }
                let result = self.response(&service, &payload);
                self.results.insert(request, result);
                Ok(Response::Accepted { id: request })
            }
            Command::TakeResult { request }
                if (self.fail_rtsp_settlement && self.unsettled_rtsp.contains(&request))
                    || (self.fail_application_settlement
                        && self.unsettled_application.contains(&request)) =>
            {
                Err("lost result".into())
            }
            Command::TakeResult { request } => Ok(Response::Result {
                payload: self.results.remove(&request).expect("queued result"),
            }),
            Command::Cancel { request } | Command::ReleaseResult { request }
                if (self.fail_rtsp_settlement && self.unsettled_rtsp.contains(&request))
                    || (self.fail_application_settlement
                        && self.unsettled_application.contains(&request)) =>
            {
                Ok(Response::error("unavailable", "fixture settlement failure"))
            }
            Command::Cancel { .. } | Command::ReleaseResult { .. } => Ok(Response::Ok),
            _ => Ok(Response::error("unexpected", "unexpected command")),
        }
    }

    fn response(&mut self, service: &str, payload: &Value) -> Value {
        let action = payload["action"].as_str().unwrap_or_default();
        if service == "amaterasu.rtsp/1" && action == "open" {
            self.rtsp_snapshot = payload.get("frame_limiter_settings").cloned();
            self.rtsp_executable_path = payload["frame_limiter_executable_path"]
                .as_str()
                .map(str::to_owned);
        }
        if service == "amaterasu.rtsp/1"
            && self
                .unavailable_output
                .as_deref()
                .is_some_and(|output| action == "open" && payload["capture"]["output"] == output)
        {
            return json!({"ok":false,"code":"not_found"});
        }
        if service == "amaterasu.rtsp/1"
            && self
                .unavailable_audio_endpoint
                .as_deref()
                .is_some_and(|endpoint| {
                    action == "open" && payload["capture"]["audio_endpoint"] == endpoint
                })
        {
            return json!({"ok":false,"code":"not_found"});
        }
        if service == "amaterasu.rtsp/1" && self.unavailable_output.is_some() && action == "lookup"
        {
            return json!({"ok":false,"code":"ticket_not_found"});
        }
        if service == "amaterasu.launch/1" {
            if action == "preflight_export" {
                let launch = self.response(
                    service,
                    &json!({
                        "action":"launch_export",
                        "operation_id":payload["operation_id"],
                        "app_id":payload["app_id"],
                    }),
                );
                return json!({
                    "ok":true,
                    "preflight_token":"fixture-preflight-token",
                    "app_id":payload["app_id"],
                    "catalog_revision":1,
                    "config_overrides":launch["launch"]["config_overrides"].as_object()
                        .map(|overrides| json!(overrides)).unwrap_or_else(|| json!({})),
                    "executable_path":self.application_executable_path,
                });
            }
            if action == "launch_export" {
                self.application_operation = payload["operation_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                self.application_id = payload["app_id"].as_str().unwrap_or_default().to_owned();
                self.application_snapshot = payload.get("frame_limiter_settings").cloned();
            }
            let operation = self.application_operation.as_str();
            let app = self.application_id.as_str();
            let launch_id = if self.wrong_application_id { 99 } else { 41 };
            let state = if action == "stop" || (action == "status" && self.application_stopped) {
                "stopped"
            } else {
                self.application_state
            };
            let mut launch = json!({
                "launch_id": launch_id,
                "operation_id": operation,
                "app_id": app,
                "state": state,
                "session_id": 2,
                "process_id": 3,
                "failure": null,
                "cleanup_error": if self.application_unhealthy {
                    Value::String("fixture_failure".into())
                } else {
                    Value::Null
                },
                "catalog_revision": self.application_catalog_revision,
            });
            launch["frame_limiter_settings"] =
                self.application_snapshot.clone().unwrap_or(Value::Null);
            launch["executable_path"] = json!(self.application_executable_path);
            if self.application_fec_percentage.is_some()
                || self.application_max_bitrate.is_some()
                || self.application_minimum_fps_target.is_some()
                || self.application_video_max_batch_size_kb.is_some()
                || self.application_stream_audio.is_some()
                || self.application_audio_sink.is_some()
                || self.application_virtual_sink.is_some()
                || self.application_audio_sink_capture_only.is_some()
                || self.application_back_button_timeout.is_some()
                || self
                    .application_input_overrides
                    .as_ref()
                    .is_some_and(|overrides| {
                        overrides
                            .as_object()
                            .is_some_and(|object| !object.is_empty())
                    })
            {
                let mut config_overrides = json!({});
                if let Some(fec_percentage) = self.application_fec_percentage {
                    config_overrides["fec_percentage"] = json!(fec_percentage);
                }
                if let Some(max_bitrate) = self.application_max_bitrate {
                    config_overrides["max_bitrate"] = json!(max_bitrate);
                }
                if let Some(minimum_fps_target) = self.application_minimum_fps_target {
                    config_overrides["minimum_fps_target"] = json!(minimum_fps_target);
                }
                if let Some(video_max_batch_size_kb) = self.application_video_max_batch_size_kb {
                    config_overrides["video_max_batch_size_kb"] = json!(video_max_batch_size_kb);
                }
                if let Some(stream_audio) = self.application_stream_audio {
                    config_overrides["stream_audio"] = json!(stream_audio);
                }
                if let Some(audio_sink) = &self.application_audio_sink {
                    config_overrides["audio_sink"] = json!(audio_sink);
                }
                if let Some(virtual_sink) = &self.application_virtual_sink {
                    config_overrides["virtual_sink"] = json!(virtual_sink);
                }
                if let Some(capture_only) = self.application_audio_sink_capture_only {
                    config_overrides["audio_sink_capture_only"] = json!(capture_only);
                }
                if let Some(timeout) = self.application_back_button_timeout {
                    config_overrides["back_button_timeout"] = json!(timeout);
                }
                if let Some(input_overrides) = &self.application_input_overrides
                    && let Some(overrides) = input_overrides.as_object()
                {
                    for (key, value) in overrides {
                        config_overrides[key] = value.clone();
                    }
                }
                launch["config_overrides"] = config_overrides;
            }
            return json!({"ok":true,"launch":launch});
        }
        let state = if action == "close" && self.fail_rtsp_cleanup {
            "cleanup_failed"
        } else if action == "close" {
            "closed"
        } else if action == "status" && self.fail_rtsp_cleanup {
            "cleanup_failed"
        } else if action == "status" && self.rtsp_closed {
            "closed"
        } else {
            "ready"
        };
        let wrong_rtsp_snapshot = self.wrong_rtsp_snapshot;
        self.wrong_rtsp_snapshot = false;
        json!({
            "ok": true,
            "ticket": 71,
            "state": state,
            "local": "127.0.0.1:48010",
            "rtsp_url": "rtspenc://127.0.0.1:48010/launch-47",
            "session": 11,
            "stream_state": "prepared",
            "frame_limiter_settings": if wrong_rtsp_snapshot {
                json!({"wrong":"snapshot"})
            } else {
                self.rtsp_snapshot.clone().unwrap_or(Value::Null)
            },
            "frame_limiter_executable_path": self.rtsp_executable_path,
            "failure": null,
            "cleanup_error": if state == "cleanup_failed" {
                Value::String("fixture_failure".into())
            } else {
                Value::Null
            },
        })
    }

    fn actions(&self) -> Vec<(&str, &str)> {
        self.commands
            .iter()
            .filter_map(|command| match command {
                Command::Request {
                    service, payload, ..
                } => Some((
                    service.as_str(),
                    payload["action"].as_str().unwrap_or_default(),
                )),
                _ => None,
            })
            .collect()
    }
}

fn poll_until_reply(
    controller: &mut Controller,
    broker: &mut Broker,
    now: Instant,
) -> (u64, Reply) {
    for _ in 0..32 {
        let replies = controller.poll(now, &mut |command| broker.send(command));
        if let Some(reply) = replies.into_iter().next() {
            return reply;
        }
    }
    panic!("lifecycle did not produce a bounded reply")
}

fn launch_ready(controller: &mut Controller, broker: &mut Broker, now: Instant) {
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (connection, reply) = poll_until_reply(controller, broker, now);
    assert_eq!(connection, 10);
    assert_eq!(reply.status, 200);
    assert!(
        String::from_utf8(reply.body)
            .unwrap()
            .contains("rtspenc://")
    );
    controller.reply_delivered(connection, now);
    assert_eq!(controller.current_game(&peer(1)), 1);
}

fn opened_audio_endpoint(broker: &Broker) -> &str {
    broker
        .commands
        .iter()
        .find_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => {
                payload["capture"]["audio_endpoint"].as_str()
            }
            _ => None,
        })
        .expect("RTSP open request includes audio endpoint")
}

fn opened_audio_endpoints(broker: &Broker) -> Vec<&str> {
    broker
        .commands
        .iter()
        .filter_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => {
                payload["capture"]["audio_endpoint"].as_str()
            }
            _ => None,
        })
        .collect()
}

#[test]
fn global_app_and_verified_client_fec_values_apply_in_precedence_order() {
    for (app_fec, client_fec, expected) in [
        (None, None, 20),
        (Some(37), None, 37),
        (Some(37), Some(49), 49),
    ] {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        let mut broker = Broker {
            application_fec_percentage: app_fec,
            ..Default::default()
        };
        assert!(
            controller
                .launch(10, request(1), peer_with_fec(1, client_fec), now)
                .is_none()
        );
        let _ = poll_until_reply(&mut controller, &mut broker, now);
        let rtsp_open = broker
            .commands
            .iter()
            .find_map(|command| match command {
                Command::Request {
                    service, payload, ..
                } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => Some(payload),
                _ => None,
            })
            .expect("RTSP open request");
        assert_eq!(rtsp_open["profile"]["fec_percentage"], expected);
    }
}

#[test]
fn global_app_and_verified_client_audio_values_apply_in_precedence_order() {
    for (global, app, client, expected) in [
        (true, None, None, true),
        (false, None, None, false),
        (false, Some(true), None, true),
        (true, Some(false), None, false),
        (true, Some(false), Some(true), true),
        (false, Some(true), Some(false), false),
    ] {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        controller.set_global_stream_audio(global);
        let mut broker = Broker {
            application_stream_audio: app,
            ..Default::default()
        };
        assert!(
            controller
                .launch(10, request(1), peer_with_overrides(1, None, client), now)
                .is_none()
        );
        // Settings changes after admission apply to later launches only.
        controller.set_global_stream_audio(!global);
        let _ = poll_until_reply(&mut controller, &mut broker, now);
        let rtsp_open = broker
            .commands
            .iter()
            .find_map(|command| match command {
                Command::Request {
                    service, payload, ..
                } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => Some(payload),
                _ => None,
            })
            .expect("RTSP open request");
        assert_eq!(rtsp_open["stream_audio"], expected);
    }
}

#[test]
fn continuous_audio_is_requested_per_launch_and_requires_effective_stream_audio() {
    for (requested, global, app, client, expected) in [
        (false, true, None, None, false),
        (true, true, None, None, true),
        (true, false, None, None, false),
        (true, true, Some(false), None, false),
        (true, false, Some(true), None, true),
        (true, true, Some(false), Some(true), true),
        (true, false, Some(true), Some(false), false),
    ] {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        controller.set_global_stream_audio(global);
        let mut broker = Broker {
            application_stream_audio: app,
            ..Default::default()
        };
        let mut launch_request = request(1);
        launch_request.continuous_audio = requested;

        assert!(
            controller
                .launch(
                    10,
                    launch_request,
                    peer_with_overrides(1, None, client),
                    now,
                )
                .is_none()
        );
        let _ = poll_until_reply(&mut controller, &mut broker, now);

        let rtsp_open = opened_rtsp_payload(&broker);
        assert_eq!(
            rtsp_open["continuous_audio"], expected,
            "requested={requested}, global={global}, app={app:?}, client={client:?}"
        );
    }
}

#[test]
fn global_video_batch_size_reaches_rtsp_open_and_is_snapshotted_at_admission() {
    for (configured, later_value) in [(16, 32), (32, 64), (64, 16)] {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        controller.set_global_video_max_batch_size_kb(configured);
        let mut broker = Broker::default();

        assert!(controller.launch(10, request(1), peer(1), now).is_none());
        controller.set_global_video_max_batch_size_kb(later_value);
        let _ = poll_until_reply(&mut controller, &mut broker, now);

        let rtsp_open = broker
            .commands
            .iter()
            .find_map(|command| match command {
                Command::Request {
                    service, payload, ..
                } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => Some(payload),
                _ => None,
            })
            .expect("RTSP open request");
        assert_eq!(rtsp_open["profile"]["video_max_batch_size_kb"], configured);
    }
}

#[test]
fn global_ping_timeout_reaches_rtsp_open_and_is_snapshotted_at_admission() {
    for (configured, later_value) in [(0, 250), (250, 10_000), (i32::MAX as u32, 0)] {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        controller.set_global_ping_timeout_ms(configured);
        let mut broker = Broker::default();

        assert!(controller.launch(10, request(1), peer(1), now).is_none());
        controller.set_global_ping_timeout_ms(later_value);
        let _ = poll_until_reply(&mut controller, &mut broker, now);

        let rtsp_open = broker
            .commands
            .iter()
            .find_map(|command| match command {
                Command::Request {
                    service, payload, ..
                } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => Some(payload),
                _ => None,
            })
            .expect("RTSP open request");
        assert_eq!(rtsp_open["ping_timeout_ms"], configured);
    }
}

#[test]
fn native_pen_touch_resolves_client_before_application_and_preserves_inheritance() {
    for (marker, client, application, expected) in [
        (51, Some(false), Some(true), Some(false)),
        (52, Some(true), Some(false), Some(true)),
        (53, None, Some(false), Some(false)),
        (54, None, None, None),
    ] {
        let now = Instant::now();
        let mut broker = Broker {
            application_input_overrides: Some(json!({"native_pen_touch":application})),
            ..Broker::default()
        };
        let mut controller = Controller::new(Some(config())).unwrap();
        assert!(
            controller
                .launch(
                    10,
                    request(marker),
                    peer_with_native_pen_touch(marker, client),
                    now,
                )
                .is_none()
        );
        let _ = poll_until_reply(&mut controller, &mut broker, now);

        let rtsp_open = opened_rtsp_payload(&broker);
        assert_eq!(rtsp_open["native_pen_touch"], json!(expected));
    }
}

#[test]
fn application_ds4_touchpad_override_reaches_rtsp_open_including_false() {
    for (marker, application_value, expected) in [
        (61, Some(true), json!(true)),
        (62, Some(false), json!(false)),
        (63, None, Value::Null),
    ] {
        let now = Instant::now();
        let application_overrides = application_value
            .map(|value| json!({"ds4_back_as_touchpad_click":value}))
            .unwrap_or_else(|| json!({}));
        let mut broker = Broker {
            application_input_overrides: Some(application_overrides),
            ..Broker::default()
        };
        let mut controller = Controller::new(Some(config())).unwrap();
        assert!(
            controller
                .launch(
                    10,
                    request(marker),
                    peer_with_native_pen_touch(marker, None),
                    now,
                )
                .is_none()
        );
        let _ = poll_until_reply(&mut controller, &mut broker, now);

        let rtsp_open = opened_rtsp_payload(&broker);
        assert_eq!(rtsp_open["ds4_back_as_touchpad_click"], expected);
    }
}

#[test]
fn encryption_modes_and_authenticated_peer_ip_are_snapshotted_into_rtsp_open() {
    let now = Instant::now();
    let remote = "198.51.100.42".parse().unwrap();
    let certificate = vec![42; 32];
    let peer = PeerIdentity::new(remote, certificate.clone())
        .unwrap()
        .with_paired_identity(
            "trusted-policy-peer".into(),
            fingerprint(&certificate),
            ClientConfigOverrides::default(),
        )
        .unwrap();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_global_video_encryption_modes(2, 0);
    let mut broker = Broker::default();

    assert!(controller.launch(10, request(1), peer, now).is_none());
    controller.set_global_video_encryption_modes(0, 2);
    let _ = poll_until_reply(&mut controller, &mut broker, now);

    let rtsp_open = opened_rtsp_payload(&broker);
    assert_eq!(rtsp_open["peer_ip"], "198.51.100.42");
    assert_eq!(
        rtsp_open["video_encryption_policy"],
        json!({"lan_mode":2,"wan_mode":0})
    );
}

#[test]
fn global_max_bitrate_is_sent_as_encoder_cap_and_snapshotted_at_admission() {
    for (configured, later_value, expected) in [
        (Some(5_000), Some(8_000), Some(5_000)),
        (Some(200_000), None, Some(200_000)),
        (None, Some(5_000), None),
    ] {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        controller.set_global_max_bitrate_kbps(configured);
        let mut broker = Broker::default();

        assert!(controller.launch(10, request(1), peer(1), now).is_none());
        controller.set_global_max_bitrate_kbps(later_value);
        let _ = poll_until_reply(&mut controller, &mut broker, now);

        let rtsp_open = broker
            .commands
            .iter()
            .find_map(|command| match command {
                Command::Request {
                    service, payload, ..
                } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => Some(payload),
                _ => None,
            })
            .expect("RTSP open request");
        assert_eq!(rtsp_open["profile"]["bitrate_kbps"], 10_000);
        assert_eq!(rtsp_open["max_bitrate_kbps"], json!(expected));
    }
}

#[test]
fn stream_overrides_resolve_client_before_application_before_global() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_global_max_bitrate_kbps(Some(9_000));
    controller.set_global_minimum_fps_target(20.0);
    controller.set_global_video_max_batch_size_kb(64);
    let client = peer_with_stream_overrides(1, Some(5_000), Some(45.5), Some(16));
    let mut broker = Broker {
        application_max_bitrate: Some(6_000),
        application_minimum_fps_target: Some(30.0),
        application_video_max_batch_size_kb: Some(32),
        ..Default::default()
    };

    assert!(controller.launch(10, request(1), client, now).is_none());
    controller.set_global_max_bitrate_kbps(None);
    controller.set_global_minimum_fps_target(60.0);
    controller.set_global_video_max_batch_size_kb(64);
    let _ = poll_until_reply(&mut controller, &mut broker, now);

    let rtsp_open = opened_rtsp_payload(&broker);
    assert_eq!(rtsp_open["profile"]["bitrate_kbps"], 10_000);
    assert_eq!(rtsp_open["max_bitrate_kbps"], 5_000);
    assert_eq!(rtsp_open["minimum_fps_target"], 45.5);
    assert_eq!(rtsp_open["profile"]["video_max_batch_size_kb"], 16);
}

#[test]
fn stream_overrides_preserve_explicit_zero_and_fall_back_to_application_or_global() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_global_max_bitrate_kbps(Some(8_000));
    controller.set_global_minimum_fps_target(20.0);
    controller.set_global_video_max_batch_size_kb(16);
    let mut broker = Broker {
        application_max_bitrate: Some(6_000),
        application_minimum_fps_target: Some(30.0),
        application_video_max_batch_size_kb: Some(32),
        ..Default::default()
    };

    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let _ = poll_until_reply(&mut controller, &mut broker, now);

    let rtsp_open = opened_rtsp_payload(&broker);
    assert_eq!(rtsp_open["max_bitrate_kbps"], 6_000);
    assert_eq!(rtsp_open["minimum_fps_target"], 30.0);
    assert_eq!(rtsp_open["profile"]["video_max_batch_size_kb"], 32);

    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_global_max_bitrate_kbps(Some(8_000));
    controller.set_global_minimum_fps_target(20.0);
    controller.set_global_video_max_batch_size_kb(16);
    let client = peer_with_stream_overrides(2, Some(0), Some(0.0), Some(0));
    let mut broker = Broker {
        application_max_bitrate: Some(6_000),
        application_minimum_fps_target: Some(30.0),
        application_video_max_batch_size_kb: Some(32),
        ..Default::default()
    };

    assert!(controller.launch(10, request(2), client, now).is_none());
    let _ = poll_until_reply(&mut controller, &mut broker, now);

    let rtsp_open = opened_rtsp_payload(&broker);
    assert_eq!(rtsp_open["max_bitrate_kbps"], Value::Null);
    assert_eq!(rtsp_open["minimum_fps_target"], 0.0);
    assert_eq!(rtsp_open["profile"]["video_max_batch_size_kb"], 0);
}

#[test]
fn global_minimum_fps_target_reaches_rtsp_open_and_is_snapshotted_at_admission() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_global_minimum_fps_target(23.5);
    let mut broker = Broker::default();

    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    controller.set_global_minimum_fps_target(60.0);
    let _ = poll_until_reply(&mut controller, &mut broker, now);

    let rtsp_open = broker
        .commands
        .iter()
        .find_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => Some(payload),
            _ => None,
        })
        .expect("RTSP open request");
    assert_eq!(rtsp_open["minimum_fps_target"], 23.5);
}

#[test]
fn global_wgc_pacing_smoothing_reaches_rtsp_open_and_is_snapshotted_at_admission() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_global_wgc_pacing_smoothing(false);
    let mut broker = Broker::default();

    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    controller.set_global_wgc_pacing_smoothing(true);
    let _ = poll_until_reply(&mut controller, &mut broker, now);

    let rtsp_open = broker
        .commands
        .iter()
        .find_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => Some(payload),
            _ => None,
        })
        .expect("RTSP open request");
    assert_eq!(rtsp_open["wgc_pacing_smoothing"], false);
}

#[test]
fn global_bitrate_below_encoder_minimum_can_be_overridden_by_client_cap() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_global_max_bitrate_kbps(Some(99));
    let client = peer_with_stream_overrides(1, Some(5_000), None, None);
    let mut broker = Broker::default();

    assert!(controller.launch(10, request(1), client, now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);

    assert_eq!(reply.status, 200);
    assert_eq!(opened_rtsp_payload(&broker)["max_bitrate_kbps"], 5_000);
}

#[test]
fn global_bitrate_below_encoder_minimum_can_be_overridden_by_application_cap() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_global_max_bitrate_kbps(Some(99));
    let mut broker = Broker {
        application_max_bitrate: Some(6_000),
        ..Default::default()
    };

    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);

    assert_eq!(reply.status, 200);
    assert_eq!(opened_rtsp_payload(&broker)["max_bitrate_kbps"], 6_000);
}

#[test]
fn global_bitrate_below_encoder_minimum_can_be_cleared_by_client_override() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_global_max_bitrate_kbps(Some(99));
    let client = peer_with_stream_overrides(1, Some(0), None, None);
    let mut broker = Broker {
        application_max_bitrate: Some(6_000),
        ..Default::default()
    };

    assert!(controller.launch(10, request(1), client, now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);

    assert_eq!(reply.status, 200);
    assert_eq!(
        opened_rtsp_payload(&broker)["max_bitrate_kbps"],
        Value::Null
    );
}

#[test]
fn effective_client_bitrate_below_encoder_minimum_fails_before_rtsp_open() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_global_max_bitrate_kbps(Some(5_000));
    let client = peer_with_stream_overrides(1, Some(99), None, None);
    let mut broker = Broker::default();

    assert!(controller.launch(10, request(1), client, now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);

    assert_ne!(reply.status, 200);
    assert!(!broker.actions().contains(&("amaterasu.rtsp/1", "open")));
}

#[test]
fn effective_application_bitrate_below_encoder_minimum_fails_before_rtsp_open() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_global_max_bitrate_kbps(Some(5_000));
    let mut broker = Broker {
        application_max_bitrate: Some(99),
        ..Default::default()
    };

    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let _ = poll_until_reply(&mut controller, &mut broker, now);

    assert_eq!(
        controller
            .records
            .values()
            .next()
            .expect("launch record")
            .failure,
        Some("max_bitrate_below_encoder_minimum")
    );
    assert!(!broker.actions().contains(&("amaterasu.rtsp/1", "open")));
}

#[test]
fn global_bitrate_below_encoder_minimum_without_override_fails_before_rtsp_open() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_global_max_bitrate_kbps(Some(99));
    let mut broker = Broker::default();

    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);

    assert_ne!(reply.status, 200);
    assert!(!broker.actions().contains(&("amaterasu.rtsp/1", "open")));
}

#[test]
fn global_capture_modes_reach_rtsp_open_and_are_snapshotted_at_admission() {
    for (mode, bootstrap_provider, expected_provider, expected_constant_rate) in [
        (
            CaptureMode::Auto,
            CaptureProvider::Dxgi,
            CaptureProvider::Dxgi,
            false,
        ),
        (
            CaptureMode::Auto,
            CaptureProvider::Wgc,
            CaptureProvider::Wgc,
            false,
        ),
        (
            CaptureMode::Ddx,
            CaptureProvider::Wgc,
            CaptureProvider::Dxgi,
            false,
        ),
        (
            CaptureMode::Wgc,
            CaptureProvider::Dxgi,
            CaptureProvider::Wgc,
            false,
        ),
        (
            CaptureMode::Wgcc,
            CaptureProvider::Dxgi,
            CaptureProvider::Wgc,
            true,
        ),
    ] {
        let now = Instant::now();
        let mut stream_config = config();
        stream_config.capture.capture_provider = bootstrap_provider;
        let mut controller = Controller::new(Some(stream_config)).unwrap();
        controller.set_capture_mode(mode);
        let mut broker = Broker::default();

        assert!(controller.launch(10, request(1), peer(1), now).is_none());
        controller.set_capture_mode(CaptureMode::Ddx);
        let _ = poll_until_reply(&mut controller, &mut broker, now);

        let rtsp_open = broker
            .commands
            .iter()
            .find_map(|command| match command {
                Command::Request {
                    service, payload, ..
                } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => Some(payload),
                _ => None,
            })
            .expect("RTSP open request");
        assert_eq!(
            rtsp_open["capture"]["capture_provider"],
            json!(expected_provider)
        );
        assert_eq!(
            rtsp_open["capture"]["constant_rate"],
            expected_constant_rate
        );
    }
}

#[test]
fn success_requires_active_application_and_ready_rtsp_then_retries_same_url() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);

    let retry = controller
        .launch(11, request(1), peer(1), now)
        .expect("idempotent ready reply");
    assert_eq!(retry.status, 200);
    controller.reply_delivered(11, now);
    assert_eq!(
        broker.actions()[..3],
        [
            ("amaterasu.launch/1", "preflight_export"),
            ("amaterasu.launch/1", "launch_export"),
            ("amaterasu.rtsp/1", "open")
        ]
    );
}

#[test]
fn launch_export_uses_the_preparation_snapshot_captured_at_admission() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let original: PreparationStep = serde_json::from_value(json!({
        "run":{"executable":std::env::current_exe().unwrap(),"args":["original"]}
    }))
    .unwrap();
    let changed: PreparationStep = serde_json::from_value(json!({
        "run":{"executable":std::env::current_exe().unwrap(),"args":["changed"]}
    }))
    .unwrap();
    controller.set_global_preparation(std::slice::from_ref(&original));
    let mut broker = Broker::default();

    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    controller.set_global_preparation(std::slice::from_ref(&changed));
    let _ = poll_until_reply(&mut controller, &mut broker, now);

    let launch_payload = broker
        .commands
        .iter()
        .find_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.launch/1" && payload["action"] == "launch_export" => {
                Some(payload)
            }
            _ => None,
        })
        .expect("launch export request");
    assert_eq!(
        launch_payload["global_preparation"][0]["run"]["args"],
        json!(["original"])
    );
}

#[test]
fn launch_export_includes_admitted_frame_limiter_settings_and_context() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let original = FrameLimiterSettings {
        frame_limiter_enable: Some(true),
        frame_limiter_fps_limit: Some(144.0),
        frame_limiter_provider: Some("none".into()),
        ..Default::default()
    };
    let changed = FrameLimiterSettings {
        frame_limiter_enable: Some(false),
        frame_limiter_fps_limit: Some(72.0),
        frame_limiter_provider: Some("auto".into()),
        ..Default::default()
    };
    controller.set_global_frame_limiter_settings(&original);
    let mut launch_request = request(1);
    launch_request.fps = 60;
    let mut broker = Broker::default();

    assert!(
        controller
            .launch(10, launch_request, peer(1), now)
            .is_none()
    );
    controller.set_global_frame_limiter_settings(&changed);
    let _ = poll_until_reply(&mut controller, &mut broker, now);

    let launch_payload = broker
        .commands
        .iter()
        .find_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.launch/1" && payload["action"] == "launch_export" => {
                Some(payload)
            }
            _ => None,
        })
        .expect("launch export request");
    let expected_settings = json!({
        "version":1,
        "settings":serde_json::to_value(original.effective()).unwrap(),
        "requested_stream_fps_millihz":60000,
        "client_display_mode_override_millihz":null,
        "lossless_scaling_limit_millihz":null,
        "virtual_display_active":null,
        "physical_framegen_capture":null,
        "game_frame_generation_mode":null,
        "rtss_frame_limit_type_explicit_override":null
    });

    assert_eq!(launch_payload["frame_limiter_settings"], expected_settings);
    assert_eq!(launch_payload["client_frame_limiter_overrides"], json!({}));
    let rtsp_open = opened_rtsp_payload(&broker);
    assert!(rtsp_open["frame_limiter_settings"].is_null());
    assert!(rtsp_open["frame_limiter_executable_path"].is_null());
}

#[test]
fn paired_frame_limiter_values_override_global_settings_and_keep_sparse_presence() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let global_settings = FrameLimiterSettings {
        frame_limiter_enable: Some(true),
        frame_limiter_fps_limit: Some(120.0),
        ..Default::default()
    };
    controller.set_global_frame_limiter_settings(&global_settings);
    let mut broker = Broker::default();

    assert!(
        controller
            .launch(
                10,
                request(1),
                peer_with_frame_limiter_overrides(1, Some(false), Some(0.0)),
                now,
            )
            .is_none()
    );
    let _ = poll_until_reply(&mut controller, &mut broker, now);

    let launch_payload = broker
        .commands
        .iter()
        .find_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.launch/1" && payload["action"] == "launch_export" => {
                Some(payload)
            }
            _ => None,
        })
        .expect("launch export request");
    assert_eq!(
        launch_payload["frame_limiter_settings"]["settings"]["frame_limiter_enable"],
        false
    );
    assert_eq!(
        launch_payload["frame_limiter_settings"]["settings"]["frame_limiter_fps_limit_millihz"],
        0
    );
    assert_eq!(
        launch_payload["client_frame_limiter_overrides"],
        json!({"frame_limiter_enable":false,"frame_limiter_fps_limit":0.0})
    );
}

#[test]
fn application_frame_limiter_values_decode_sparsely_and_reject_malformed_values() {
    let payload = json!({
        "launch": {
            "config_overrides": {
                "frame_limiter_enable":false,
                "frame_limiter_fps_limit":72.0,
                "frame_limiter_provider":"auto"
            }
        }
    });
    let decoded = super::workflow::application_config_overrides(&payload).unwrap();
    assert_eq!(
        decoded.frame_limiter_settings(),
        FrameLimiterSettings {
            frame_limiter_enable: Some(false),
            frame_limiter_fps_limit: Some(72.0),
            frame_limiter_provider: Some("auto".into()),
            ..Default::default()
        }
    );

    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker {
        application_input_overrides: Some(json!({
            "frame_limiter_fps_limit":72.0,
            "frame_limiter_provider":"auto"
        })),
        ..Broker::default()
    };
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let _ = poll_until_reply(&mut controller, &mut broker, now);
    let record = controller.records.values().next().expect("launch record");
    assert_eq!(
        record.application_frame_limiter_settings,
        FrameLimiterSettings {
            frame_limiter_fps_limit: Some(72.0),
            frame_limiter_provider: Some("auto".into()),
            ..Default::default()
        }
    );
    let launch_payload = broker
        .commands
        .iter()
        .find_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.launch/1" && payload["action"] == "launch_export" => {
                Some(payload)
            }
            _ => None,
        })
        .expect("launch export");
    assert_eq!(launch_payload["client_frame_limiter_overrides"], json!({}));

    #[cfg(windows)]
    {
        let windows_payload = json!({
            "launch": {
                "config_overrides": {
                    "frame_limiter_disable_vsync":false,
                    "frame_limiter_auto_virtual_framegen":"legacy"
                }
            }
        });
        let decoded = super::workflow::application_config_overrides(&windows_payload).unwrap();
        assert_eq!(
            decoded.frame_limiter_settings(),
            FrameLimiterSettings {
                frame_limiter_disable_vsync: Some(false),
                frame_limiter_auto_virtual_framegen: Some("legacy".into()),
                ..Default::default()
            }
        );
    }

    let malformed_values = [
        json!({"frame_limiter_enable":"false"}),
        json!({"frame_limiter_fps_limit":"60"}),
        json!({"frame_limiter_fps_limit":1000.001}),
        json!({"frame_limiter_provider":[]}),
        json!({"frame_limiter_disable_vsync":1}),
        json!({"frame_limiter_auto_virtual_framegen":"invalid"}),
        json!({"rtss_frame_limit_type":false}),
        json!({"ds4_back_as_touchpad_click":"false"}),
        json!({"unrecognized_override":true}),
    ];
    for application_overrides in malformed_values.iter() {
        let payload = json!({"launch":{"config_overrides":application_overrides}});
        assert!(super::workflow::application_config_overrides(&payload).is_err());
    }

    for (offset, application_overrides) in malformed_values.into_iter().enumerate() {
        let now = Instant::now();
        let marker = 70 + offset as u8;
        let mut controller = Controller::new(Some(config())).unwrap();
        let mut broker = Broker {
            application_input_overrides: Some(application_overrides),
            ..Broker::default()
        };

        assert!(
            controller
                .launch(10, request(marker), peer(marker), now)
                .is_none()
        );
        let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
        assert_eq!(reply.status, 503);
        assert!(!broker.commands.iter().any(|command| matches!(
            command,
            Command::Request { service, payload, .. }
                if service == "amaterasu.rtsp/1" && payload["action"] == "open"
        )));
    }
}

#[cfg(windows)]
#[test]
fn application_rtss_frame_limit_type_decodes_windows_domain_values() {
    for frame_limit_type in [
        "async",
        "front edge sync",
        "back edge sync",
        "nvidia reflex",
    ] {
        let payload = json!({
            "launch": {
                "config_overrides": {
                    "rtss_frame_limit_type":frame_limit_type
                }
            }
        });
        let decoded = super::workflow::application_config_overrides(&payload).unwrap();
        assert_eq!(
            decoded
                .frame_limiter_settings()
                .rtss_frame_limit_type
                .as_deref(),
            Some(frame_limit_type)
        );
    }

    let invalid_payload = json!({
        "launch": {
            "config_overrides": {
                "rtss_frame_limit_type":"sync"
            }
        }
    });
    assert!(super::workflow::application_config_overrides(&invalid_payload).is_err());
    assert_eq!(
        FrameLimiterSettings {
            rtss_frame_limit_type: Some("sync".into()),
            ..Default::default()
        }
        .validate(),
        Err("invalid_rtss_frame_limit_type")
    );
}

#[cfg(not(windows))]
#[test]
fn application_rtss_frame_limit_type_rejects_windows_domain_values_off_windows() {
    let payload = json!({
        "launch": {
            "config_overrides": {
                "rtss_frame_limit_type":"async"
            }
        }
    });
    assert!(super::workflow::application_config_overrides(&payload).is_err());
    assert_eq!(
        FrameLimiterSettings {
            rtss_frame_limit_type: Some("async".into()),
            ..Default::default()
        }
        .validate(),
        Err("unsupported_frame_limiter_setting")
    );
}

#[test]
fn frame_limiter_fields_resolve_client_then_application_then_global_independently() {
    let global = FrameLimiterSettings {
        frame_limiter_enable: Some(true),
        frame_limiter_fps_limit: Some(60.0),
        frame_limiter_provider: Some("auto".into()),
        frame_limiter_disable_vsync: Some(true),
        frame_limiter_auto_virtual_framegen: Some("enabled".into()),
        ..Default::default()
    };
    let application = FrameLimiterSettings {
        frame_limiter_enable: Some(true),
        frame_limiter_fps_limit: Some(90.0),
        frame_limiter_provider: Some("rtss".into()),
        frame_limiter_disable_vsync: Some(false),
        frame_limiter_auto_virtual_framegen: Some("legacy".into()),
        ..Default::default()
    };
    let client = FrameLimiterSettings {
        frame_limiter_enable: Some(false),
        frame_limiter_fps_limit: Some(144.0),
        frame_limiter_provider: Some("none".into()),
        frame_limiter_disable_vsync: Some(true),
        frame_limiter_auto_virtual_framegen: Some("vrr".into()),
        ..Default::default()
    };
    let resolved = super::workflow::resolve_frame_limiter_settings(&global, &application, &client);
    assert_eq!(resolved.frame_limiter_enable, Some(false));
    assert_eq!(resolved.frame_limiter_fps_limit, Some(144.0));
    assert_eq!(resolved.frame_limiter_provider.as_deref(), Some("none"));
    assert_eq!(resolved.frame_limiter_disable_vsync, Some(true));
    assert_eq!(
        resolved.frame_limiter_auto_virtual_framegen.as_deref(),
        Some("vrr")
    );

    let application_resolved = super::workflow::resolve_frame_limiter_settings(
        &global,
        &application,
        &FrameLimiterSettings::default(),
    );
    assert_eq!(application_resolved.frame_limiter_enable, Some(true));
    assert_eq!(application_resolved.frame_limiter_fps_limit, Some(90.0));
    assert_eq!(
        application_resolved.frame_limiter_provider.as_deref(),
        Some("rtss")
    );
    assert_eq!(
        application_resolved.frame_limiter_disable_vsync,
        Some(false)
    );
    assert_eq!(
        application_resolved
            .frame_limiter_auto_virtual_framegen
            .as_deref(),
        Some("legacy")
    );

    let global_resolved = super::workflow::resolve_frame_limiter_settings(
        &global,
        &FrameLimiterSettings::default(),
        &FrameLimiterSettings::default(),
    );
    assert_eq!(global_resolved.frame_limiter_enable, Some(true));
    assert_eq!(global_resolved.frame_limiter_fps_limit, Some(60.0));
    assert_eq!(
        global_resolved.frame_limiter_provider.as_deref(),
        Some("auto")
    );
    assert_eq!(global_resolved.frame_limiter_disable_vsync, Some(true));
    assert_eq!(
        global_resolved
            .frame_limiter_auto_virtual_framegen
            .as_deref(),
        Some("enabled")
    );

    let defaults = super::workflow::resolve_frame_limiter_settings(
        &FrameLimiterSettings::default(),
        &FrameLimiterSettings::default(),
        &FrameLimiterSettings::default(),
    );
    assert_eq!(defaults.frame_limiter_enable, None);
    assert_eq!(defaults.frame_limiter_fps_limit, None);
    assert_eq!(defaults.frame_limiter_provider, None);
    assert_eq!(defaults.frame_limiter_disable_vsync, None);
    assert_eq!(defaults.frame_limiter_auto_virtual_framegen, None);
}

#[cfg(windows)]
#[test]
fn rtss_frame_limit_type_resolves_client_then_application_then_global_then_default() {
    let global = FrameLimiterSettings {
        rtss_frame_limit_type: Some("async".into()),
        ..Default::default()
    };
    let application = FrameLimiterSettings {
        rtss_frame_limit_type: Some("front edge sync".into()),
        ..Default::default()
    };
    let client = FrameLimiterSettings {
        rtss_frame_limit_type: Some("nvidia reflex".into()),
        ..Default::default()
    };

    let client_resolved =
        super::workflow::resolve_frame_limiter_settings(&global, &application, &client);
    assert_eq!(
        client_resolved.rtss_frame_limit_type.as_deref(),
        Some("nvidia reflex")
    );

    let application_resolved = super::workflow::resolve_frame_limiter_settings(
        &global,
        &application,
        &FrameLimiterSettings::default(),
    );
    assert_eq!(
        application_resolved.rtss_frame_limit_type.as_deref(),
        Some("front edge sync")
    );

    let global_resolved = super::workflow::resolve_frame_limiter_settings(
        &global,
        &FrameLimiterSettings::default(),
        &FrameLimiterSettings::default(),
    );
    assert_eq!(
        global_resolved.rtss_frame_limit_type.as_deref(),
        Some("async")
    );

    let defaults = super::workflow::resolve_frame_limiter_settings(
        &FrameLimiterSettings::default(),
        &FrameLimiterSettings::default(),
        &FrameLimiterSettings::default(),
    );
    assert_eq!(defaults.rtss_frame_limit_type, None);
    assert_eq!(defaults.effective().rtss_frame_limit_type, "async");
}

#[cfg(windows)]
#[test]
fn application_rtss_frame_limit_type_is_captured_after_launch_export() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let global_settings = FrameLimiterSettings {
        rtss_frame_limit_type: Some("async".into()),
        ..Default::default()
    };
    controller.set_global_frame_limiter_settings(&global_settings);
    let mut broker = Broker {
        application_input_overrides: Some(json!({
            "rtss_frame_limit_type":"back edge sync"
        })),
        application_executable_path: Some("C:\\Games\\Example.exe".into()),
        ..Broker::default()
    };

    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(reply.status, 200);

    let record = controller.records.values().next().expect("launch record");
    assert_eq!(
        record
            .application_frame_limiter_settings
            .rtss_frame_limit_type
            .as_deref(),
        Some("back edge sync")
    );
    let actions = broker.actions();
    assert!(
        actions
            .iter()
            .position(|(_, action)| *action == "preflight_export")
            < actions
                .iter()
                .position(|(_, action)| *action == "launch_export")
    );
    let launch_payload = broker
        .commands
        .iter()
        .find_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.launch/1" && payload["action"] == "launch_export" => {
                Some(payload)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(launch_payload["preflight_token"], "fixture-preflight-token");
    assert_eq!(
        launch_payload["frame_limiter_settings"]["settings"]["rtss_frame_limit_type"],
        "back edge sync"
    );
    assert_eq!(
        launch_payload["frame_limiter_settings"]["rtss_frame_limit_type_explicit_override"],
        true
    );
    assert_eq!(
        opened_rtsp_payload(&broker)["frame_limiter_settings"],
        launch_payload["frame_limiter_settings"]
    );
    assert_eq!(
        opened_rtsp_payload(&broker)["frame_limiter_executable_path"],
        "C:\\Games\\Example.exe"
    );
    let resolved = super::workflow::resolve_frame_limiter_settings(
        &global_settings,
        &record.application_frame_limiter_settings,
        &FrameLimiterSettings::default(),
    );
    assert_eq!(
        resolved.rtss_frame_limit_type.as_deref(),
        Some("back edge sync")
    );
}

#[test]
fn malformed_preflight_override_fails_before_application_launch() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker {
        application_input_overrides: Some(json!({"frame_limiter_enable":"false"})),
        ..Broker::default()
    };
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_ne!(reply.status, 200);
    assert!(
        broker
            .actions()
            .contains(&("amaterasu.launch/1", "preflight_export"))
    );
    assert!(
        !broker
            .actions()
            .contains(&("amaterasu.launch/1", "launch_export"))
    );
}

#[test]
fn changed_application_policy_after_preflight_never_opens_rtsp() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker {
        application_fec_percentage: Some(31),
        ..Broker::default()
    };
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    for _ in 0..8 {
        controller.poll(now, &mut |command| broker.send(command));
        if controller
            .records
            .values()
            .any(|record| record.stage == Stage::LaunchApplication)
        {
            break;
        }
    }
    assert!(
        controller
            .records
            .values()
            .any(|record| record.stage == Stage::LaunchApplication)
    );
    broker.application_fec_percentage = Some(32);
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_ne!(reply.status, 200);
    assert!(!broker.actions().contains(&("amaterasu.rtsp/1", "open")));
}

#[test]
fn changed_catalog_revision_after_preflight_stops_application_before_rtsp() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    for _ in 0..8 {
        controller.poll(now, &mut |command| broker.send(command));
        if controller
            .records
            .values()
            .any(|record| record.stage == Stage::LaunchApplication)
        {
            break;
        }
    }
    assert!(
        controller
            .records
            .values()
            .any(|record| record.application_catalog_revision == Some(1))
    );

    broker.application_catalog_revision = 2;
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_ne!(reply.status, 200);
    assert!(broker.actions().contains(&("amaterasu.launch/1", "stop")));
    assert!(!broker.actions().contains(&("amaterasu.rtsp/1", "open")));
}

#[test]
fn rtsp_snapshot_readback_mismatch_never_completes_launch() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker {
        application_executable_path: Some("C:\\Games\\Example.exe".into()),
        wrong_rtsp_snapshot: true,
        ..Broker::default()
    };
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_ne!(reply.status, 200);
    assert!(broker.actions().contains(&("amaterasu.rtsp/1", "open")));
    assert_eq!(controller.current_game(&peer(1)), 0);
}

#[test]
fn expired_preflight_settles_without_scheduling_application_cleanup() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    assert!(
        controller
            .poll(now, &mut |command| broker.send(command))
            .is_empty()
    );
    let expired = now + std::time::Duration::from_secs(41);
    assert!(
        controller
            .poll(expired, &mut |command| broker.send(command))
            .is_empty()
    );
    let replies = controller.poll(expired, &mut |command| broker.send(command));
    assert_eq!(replies.len(), 1);
    assert_ne!(replies[0].1.status, 200);
    assert!(
        !broker
            .actions()
            .contains(&("amaterasu.launch/1", "launch_export"))
    );
    assert!(!broker.actions().contains(&("amaterasu.launch/1", "lookup")));
}

#[test]
fn response_loss_before_first_flush_stops_rtsp_and_application() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (connection, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(reply.status, 200);
    controller.connection_closed(connection, now);
    for _ in 0..24 {
        controller.poll(now, &mut |command| broker.send(command));
    }
    let actions = broker.actions();
    assert!(actions.contains(&("amaterasu.rtsp/1", "close")));
    assert!(actions.contains(&("amaterasu.launch/1", "stop")));
    assert_eq!(controller.current_game(&peer(1)), 0);
}

#[test]
fn rtsp_cleanup_failure_still_stops_application_and_retains_failure() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    broker.fail_rtsp_cleanup = true;

    assert!(controller.cancel(20, &peer(1), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(reply.status, 503);
    assert!(broker.actions().contains(&("amaterasu.launch/1", "stop")));
    assert!(controller.busy(), "unconfirmed RTSP cleanup remains owned");
}

#[test]
fn malformed_or_wrong_provider_identity_never_becomes_ready() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    broker.wrong_application_id = true;
    for _ in 0..4 {
        controller.poll(now, &mut |command| broker.send(command));
    }
    assert_eq!(controller.current_game(&peer(1)), 0);
}

#[test]
fn unhealthy_application_response_adopts_the_owned_id_then_cleans_it() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker {
        application_unhealthy: true,
        ..Broker::default()
    };
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(reply.status, 503);
    assert!(broker.actions().contains(&("amaterasu.launch/1", "stop")));
    assert_eq!(controller.current_game(&peer(1)), 0);
}

#[test]
fn lost_rtsp_admission_is_settled_before_owned_lookup_without_reopening() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker {
        lose_rtsp_open_once: true,
        application_executable_path: Some("C:\\Games\\Example.exe".into()),
        ..Broker::default()
    };
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(reply.status, 200);

    let rtsp_requests: Vec<_> = broker
        .commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| match command {
            Command::Request {
                service,
                payload,
                operation_id,
                ..
            } if service == "amaterasu.rtsp/1" => {
                Some((index, payload.clone(), operation_id.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(rtsp_requests.len(), 3);
    assert_eq!(rtsp_requests[0].1, rtsp_requests[1].1);
    assert_eq!(
        rtsp_requests[0].1["frame_limiter_settings"],
        broker.rtsp_snapshot.clone().expect("frozen RTSP snapshot")
    );
    assert_eq!(rtsp_requests[0].2, rtsp_requests[1].2);
    assert_eq!(rtsp_requests[2].1["action"], "lookup");
    let cancel = broker
        .commands
        .iter()
        .position(|command| matches!(command, Command::Cancel { .. }))
        .expect("cancel uncertain admission");
    let release = broker
        .commands
        .iter()
        .position(|command| matches!(command, Command::ReleaseResult { .. }))
        .expect("release uncertain admission");
    assert!(rtsp_requests[1].0 < cancel && cancel < release && release < rtsp_requests[2].0);
}

#[test]
fn natural_application_exit_closes_the_stream_and_releases_the_application() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    broker.application_state = "stopped";
    for _ in 0..24 {
        controller.poll(now, &mut |command| broker.send(command));
    }
    let actions = broker.actions();
    assert!(actions.contains(&("amaterasu.rtsp/1", "close")));
    assert!(actions.contains(&("amaterasu.launch/1", "stop")));
    assert_eq!(controller.current_game(&peer(1)), 0);
}

#[test]
fn repeated_cancel_and_revocation_do_not_extend_cleanup_deadlines() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);

    assert!(controller.cancel(20, &peer(1), now).is_none());
    let deadline = controller.records.values().next().unwrap().deadline;
    assert_eq!(
        controller
            .cancel(21, &peer(1), now + Duration::from_secs(1))
            .unwrap()
            .status,
        409
    );
    assert_eq!(
        controller.records.values().next().unwrap().deadline,
        deadline
    );
    controller.revoke_untrusted(now + Duration::from_secs(2), |_| false);
    controller.revoke_untrusted(now + Duration::from_secs(3), |_| false);
    assert_eq!(
        controller.records.values().next().unwrap().deadline,
        deadline
    );
}

#[test]
fn dual_uncertain_cleanup_calls_are_both_retained_and_reconciled_before_success() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    broker.fail_rtsp_settlement = true;
    broker.fail_application_settlement = true;
    assert!(controller.cancel(20, &peer(1), now).is_none());

    let (_, failed) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(failed.status, 503);
    let record = controller.records.values().next().unwrap();
    assert_eq!(record.uncertain.len(), 2);
    let deadline = record.deadline;

    broker.fail_rtsp_settlement = false;
    broker.fail_application_settlement = false;
    assert!(
        controller
            .cancel(21, &peer(1), now + Duration::from_secs(1))
            .is_none()
    );
    let (_, completed) =
        poll_until_reply(&mut controller, &mut broker, now + Duration::from_secs(1));
    assert_eq!(completed.status, 200);
    let record = controller.records.values().next().unwrap();
    assert!(record.uncertain.is_empty());
    assert!(record.state.terminal());
    assert!(
        record.deadline > deadline,
        "explicit retry gets one fresh budget"
    );
}
