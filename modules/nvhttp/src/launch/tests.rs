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
            fec_percentage,
            stream_audio,
            audio_sink,
        )
        .unwrap()
}

fn request(key: u8) -> LaunchRequest {
    LaunchRequest {
        app_id: 1,
        width: 1280,
        height: 720,
        fps: 60,
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
        peer.with_paired_identity("moonlight-id".into(), "00".repeat(32), None, None, None)
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
                None,
                None,
                Some("bad\nendpoint".into()),
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
            Some(37),
            Some(false),
            None,
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
    application_stream_audio: Option<bool>,
    application_audio_sink: Option<String>,
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
            application_stream_audio: None,
            application_audio_sink: None,
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
            if action == "launch_export" {
                self.application_operation = payload["operation_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                self.application_id = payload["app_id"].as_str().unwrap_or_default().to_owned();
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
            });
            if self.application_fec_percentage.is_some()
                || self.application_stream_audio.is_some()
                || self.application_audio_sink.is_some()
            {
                let mut config_overrides = json!({});
                if let Some(fec_percentage) = self.application_fec_percentage {
                    config_overrides["fec_percentage"] = json!(fec_percentage);
                }
                if let Some(stream_audio) = self.application_stream_audio {
                    config_overrides["stream_audio"] = json!(stream_audio);
                }
                if let Some(audio_sink) = &self.application_audio_sink {
                    config_overrides["audio_sink"] = json!(audio_sink);
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
        json!({
            "ok": true,
            "ticket": 71,
            "state": state,
            "local": "127.0.0.1:48010",
            "rtsp_url": "rtspenc://127.0.0.1:48010/launch-47",
            "session": 11,
            "stream_state": "prepared",
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
fn global_max_bitrate_caps_rtsp_profile_and_is_snapshotted_at_admission() {
    for (configured, later_value, expected) in [
        (Some(5_000), Some(8_000), 5_000),
        (Some(200_000), None, 10_000),
        (None, Some(5_000), 10_000),
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
        assert_eq!(rtsp_open["profile"]["bitrate_kbps"], expected);
    }
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
fn maximum_bitrate_below_encoder_minimum_fails_before_launch_work() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_global_max_bitrate_kbps(Some(99));

    let reply = controller
        .launch(10, request(1), peer(1), now)
        .expect("unsupported configured cap rejects launch");
    assert_eq!(reply.status, 503);
    assert!(
        String::from_utf8(reply.body)
            .unwrap()
            .contains("below the supported 100 kbps encoder minimum")
    );
    assert!(!controller.busy());
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
        broker.actions()[..2],
        [
            ("amaterasu.launch/1", "launch_export"),
            ("amaterasu.rtsp/1", "open")
        ]
    );
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
