use super::*;
use crate::encoder_snapshot::{CodecAdvertisement, EncoderSnapshot};
use crate::launch::{CaptureConfig, CaptureProvider, StreamProfile, StreamingConfig};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn paired_client_display_mode_decodes_only_as_optional_string() {
    let parsed: ClientConfigOverrides =
        serde_json::from_value(json!({"display_mode":"1920x1080x59.94"})).unwrap();
    assert_eq!(parsed.display_mode.as_deref(), Some("1920x1080x59.94"));
    assert!(
        serde_json::from_value::<ClientConfigOverrides>(json!({"display_mode":59.94})).is_err()
    );
    assert!(
        serde_json::from_value::<ClientConfigOverrides>(json!({"display_mode":null}))
            .unwrap()
            .display_mode
            .is_none()
    );
}

#[test]
fn paired_client_capture_mode_decodes_only_as_a_supported_capture_mode() {
    let parsed: ClientConfigOverrides =
        serde_json::from_value(json!({"capture":"pipewire"})).unwrap();
    assert_eq!(parsed.capture, Some(crate::launch::CaptureMode::PipeWire));
    assert!(
        serde_json::from_value::<ClientConfigOverrides>(json!({"capture":"fallback"})).is_err()
    );
    assert!(serde_json::from_value::<ClientConfigOverrides>(json!({"capture":7})).is_err());
}

#[derive(Default)]
struct Broker {
    next: u64,
    requests: BTreeMap<u64, Value>,
    services: BTreeMap<u64, String>,
    accepted: Vec<Value>,
    cancelled: BTreeSet<u64>,
    approved: bool,
    pause_begin: bool,
    /// Last exported application launch and RTSP session, replayed by the
    /// status and teardown calls of a cleanup.
    launch_template: Option<Value>,
    rtsp_template: Option<Value>,
    cleanup_started: bool,
    unpair_misses: bool,
}

impl Broker {
    fn send(&mut self, command: Command) -> ModuleResult<Response> {
        Ok(match command {
            Command::Request {
                service, payload, ..
            } => {
                assert!(matches!(
                    service.as_str(),
                    PAIRING_SERVICE
                        | "amaterasu.encode/1"
                        | "amaterasu.input/1"
                        | "amaterasu.launch/1"
                        | "amaterasu.rtsp/1"
                        | "amaterasu.stream-session/1"
                ));
                self.next += 1;
                self.accepted.push(payload.clone());
                self.services.insert(self.next, service);
                self.requests.insert(self.next, payload);
                Response::Accepted { id: self.next }
            }
            Command::TakeResult { request } => {
                if self.cancelled.contains(&request) {
                    return Ok(Response::Error {
                        code: "cancelled".into(),
                        message: String::new(),
                    });
                }
                let payload = &self.requests[&request];
                if self.pause_begin && payload["action"] == "begin" {
                    return Ok(Response::Pending);
                }
                let service = &self.services[&request];
                let key = (
                    service.clone(),
                    payload["action"].as_str().unwrap().to_owned(),
                );
                let result = match (key.0.as_str(), key.1.as_str()) {
                    ("amaterasu.input/1", "settings_get") => json!({
                        "ok":true,
                        "persisted":true,
                        "revision":1,
                        "values":{"proton_dualsense_compatibility":true}
                    }),
                    ("amaterasu.stream-session/1", "reserve_history_activity") => json!({
                        "ok":true,"reserved":true,"activity_id":payload["activity_id"],
                        "rtsp_operation_id":payload["rtsp_operation_id"]
                    }),
                    ("amaterasu.stream-session/1", "release_history_activity") => json!({
                        "ok":true,"released":true,"activity_id":payload["activity_id"],
                        "rtsp_operation_id":payload["rtsp_operation_id"]
                    }),
                    (PAIRING_SERVICE, "identity") => {
                        json!({"ok":true,"host_id":"12345678-1234-4234-8234-123456789abc"})
                    }
                    (PAIRING_SERVICE, "check_client_certificate") => {
                        let encoded = payload["certificate_pem"].as_str().unwrap();
                        let (_, certificate_der) =
                            pem_rfc7468::decode_vec(encoded.as_bytes()).unwrap();
                        json!({
                            "ok": true,
                            "paired": 1,
                            "certificate_id": hex(&Sha256::digest(certificate_der)),
                            "client_id": "certificate-bound-client",
                            "config_overrides": {
                                "fec_percentage": 49,
                                "stream_audio": false,
                                "audio_sink": "endpoint.client",
                                "audio_sink_capture_only": true,
                                "back_button_timeout": -1,
                                "controller": false,
                                "gamepad": "auto",
                                "keyboard": false,
                                "mouse": true,
                                "ds4_back_as_touchpad_click": false
                            }
                        })
                    }
                    (PAIRING_SERVICE, "begin") => {
                        json!({"ok":true,"pending":true,"pairing_id":"test-private-pairing-id"})
                    }
                    (PAIRING_SERVICE, "get_server_cert") if !self.approved => {
                        json!({"ok":true,"pending":true,"paired":0})
                    }
                    (PAIRING_SERVICE, "get_server_cert") => {
                        json!({"ok":true,"paired":1,"plaincert":hex(pem().as_bytes())})
                    }
                    (PAIRING_SERVICE, "client_challenge") => {
                        json!({"ok":true,"paired":1,"challengeresponse":"00".repeat(48)})
                    }
                    (PAIRING_SERVICE, "server_challenge_response") => {
                        json!({"ok":true,"paired":1,"pairingsecret":"00".repeat(272)})
                    }
                    (PAIRING_SERVICE, "client_pairing_secret") => {
                        json!({"ok":true,"paired":1,"certificate_id":"00".repeat(32)})
                    }
                    (PAIRING_SERVICE, "cancel") => json!({"ok":true}),
                    (PAIRING_SERVICE, "unpair_presented_client") => json!({
                        "ok":true,
                        "unpaired":if self.unpair_misses { 0 } else { 1 },
                        "certificate_id":"00".repeat(32)
                    }),
                    ("amaterasu.launch/1", "preflight_export") => json!({
                        "ok":true,
                        "preflight_token":"fixture-preflight-token",
                        "cleanup_budget_ms":20_000,
                        "app_id":payload["app_id"],
                        "catalog_revision":0,
                        "config_overrides":{
                            "fec_percentage":37,
                            "stream_audio":true,
                            "audio_sink":"endpoint.app",
                            "audio_sink_capture_only":false,
                            "back_button_timeout":1200,
                            "controller":true,
                            "gamepad":"auto",
                            "keyboard":true,
                            "mouse":false,
                            "ds4_back_as_touchpad_click":true
                        }
                    }),
                    ("amaterasu.launch/1", "launch_export") => json!({
                        "ok": true,
                        "launch": {
                            "launch_id": 1,
                            "operation_id": payload["operation_id"],
                            "app_id": payload["app_id"],
                            "state": "active",
                            "failure": null,
                            "cleanup_error": null,
                            "cleanup_budget_ms":20_000,
                            "frame_limiter_settings":payload["frame_limiter_settings"],
                            "catalog_revision": 0,
                            "application_display_overrides": {},
                            "config_overrides": {
                                "fec_percentage": 37,
                                "stream_audio": true,
                                "audio_sink": "endpoint.app",
                                "audio_sink_capture_only": false,
                                "back_button_timeout": 1200,
                                "controller": true,
                                "gamepad": "auto",
                                "keyboard": true,
                                "mouse": false,
                                "ds4_back_as_touchpad_click":true
                            }
                        }
                    }),
                    ("amaterasu.rtsp/1", "launch_option_capabilities") => {
                        fixture_option_capabilities()
                    }
                    ("amaterasu.rtsp/1", "open") => json!({
                        "ok": true,
                        "ticket": 1,
                        "encoder":payload["capture"]["encoder"],"sw_preset":payload["sw_preset"],"sw_tune":payload["sw_tune"],
                        "min_threads":payload["min_threads"],
                        "qp":payload["qp"],
                        "nvenc_settings":payload["nvenc_settings"],
                        "nvenc_spatial_aq":payload["nvenc_spatial_aq"],
                        "rtsp_url": "rtspenc://127.0.0.1:48010",
                        "state": "ready",
                        "frame_limiter_settings": payload["frame_limiter_settings"],
                        "frame_limiter_executable_path": payload["frame_limiter_executable_path"],
                        "display_policy_snapshot": payload["display_policy_snapshot"],
                        "display_request": payload["display_request"],
                        "failure": null,
                        "cleanup_error": null
                    }),
                    ("amaterasu.launch/1", "stop" | "status") => {
                        let mut launch = self.launch_template.clone().expect("exported launch");
                        launch["state"] = json!(if self.cleanup_started {
                            "stopped"
                        } else {
                            "active"
                        });
                        json!({"ok":true,"launch":launch})
                    }
                    ("amaterasu.rtsp/1", "close" | "status") => {
                        self.cleanup_started |= key.1 == "close";
                        let mut session = self.rtsp_template.clone().expect("opened RTSP session");
                        session["state"] = json!(if self.cleanup_started {
                            "closed"
                        } else {
                            "ready"
                        });
                        session
                    }
                    (service, action) => panic!("unexpected service operation {service} {action}"),
                };
                match (key.0.as_str(), key.1.as_str()) {
                    ("amaterasu.launch/1", "launch_export") => {
                        self.launch_template = Some(result["launch"].clone());
                    }
                    ("amaterasu.rtsp/1", "open") => self.rtsp_template = Some(result.clone()),
                    _ => {}
                }
                self.requests.remove(&request);
                self.services.remove(&request);
                Response::Result { payload: result }
            }
            Command::Cancel { request } => {
                self.cancelled.insert(request);
                Response::Ok
            }
            other => panic!("unexpected runtime command {other:?}"),
        })
    }
}

fn router() -> Router {
    Router::new(
        RouterConfig {
            hostname: "Amaterasu & <test>".into(),
            http_port: 47989,
            https_port: 47984,
        },
        LaunchController::new(None).unwrap(),
    )
}

fn streaming_router() -> Router {
    streaming_router_with_desktop(false)
}

fn streaming_router_with_desktop(enabled: bool) -> Router {
    Router::new(
        RouterConfig {
            hostname: "Amaterasu".into(),
            http_port: 47989,
            https_port: 47984,
        },
        LaunchController::new(Some(StreamingConfig {
            capture: CaptureConfig {
                capture_provider: CaptureProvider::Dxgi,
                output: "display-one".into(),
                encoder: amaterasu_module_sdk::EncoderSelection::Index(0),
                audio_endpoint: "default".into(),
            },
            desktop: crate::launch::DesktopSettings { enabled },
            profile: StreamProfile {
                codec: amaterasu_media::VideoCodec::H264,
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
        }))
        .unwrap(),
    )
}

fn pem() -> String {
    pem_rfc7468::encode_string("CERTIFICATE", pem_rfc7468::LineEnding::LF, &[1, 2, 3]).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn begin_target() -> String {
    format!(
        "/pair?uniqueid=client-1&phrase=getservercert&salt={}&clientcert={}&devicename=Living+room&uuid=request-uuid",
        "00".repeat(16),
        hex(pem().as_bytes())
    )
}

fn request(target: &str) -> Request {
    Request {
        method: "GET".into(),
        target: target.into(),
        body: Vec::new(),
    }
}

fn dispatch(
    router: &mut Router,
    broker: &mut Broker,
    connection: u64,
    target: &str,
    now: Instant,
) -> Option<Reply> {
    router.dispatch_with(
        connection,
        request(target),
        peer("127.0.0.1", false, None, false),
        now,
        &mut |command| broker.send(command),
    )
}

fn poll(router: &mut Router, broker: &mut Broker, now: Instant) -> Vec<(u64, Reply)> {
    router.poll_with(now, &mut |command| broker.send(command))
}

fn ready(router: &mut Router, broker: &mut Broker, now: Instant) {
    broker.approved = true;
    assert!(dispatch(router, broker, 1, &begin_target(), now).is_none());
    for _ in 0..3 {
        assert!(poll(router, broker, now).is_empty());
    }
    let result = poll(router, broker, now);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].1.status, 200);
}

#[test]
fn query_rejects_duplicate_escaped_fields_and_bounded_malformed_input() {
    for target in [
        "/pair?uniqueid=a&unique%69d=b",
        "/pair?uniqueid=a&phase=%",
        "/pair?uniqueid=%00",
        "/pair?devicename=%C2%85",
        "/pair?uniqueid=%GG",
        "/pair?uniqueid=%ff",
        "/pair?missingequals",
        "https://attacker/pair?uniqueid=a",
    ] {
        assert!(Query::parse(target).is_err(), "{target}");
    }
    assert!(Query::parse(&format!("/pair?x={}", "a".repeat(query::MAX_TARGET))).is_err());
    let fields = (0..25)
        .map(|index| format!("x{index}=y"))
        .collect::<Vec<_>>()
        .join("&");
    assert!(Query::parse(&format!("/pair?{fields}")).is_err());
    let parsed = Query::parse("/pair?devicename=Living+room%26TV&uniqueid=client").unwrap();
    assert_eq!(parsed.get("devicename"), Some("Living room&TV"));
}

#[test]
fn xml_escapes_metadata_and_advertises_configured_streaming_honestly() {
    assert_eq!(xml::escape("<&>\"'"), "&lt;&amp;&gt;&quot;&apos;");
    let response = xml::server_info(
        &router().config,
        "host-id",
        false,
        0,
        false,
        false,
        CodecAdvertisement::NONE,
    );
    let text = String::from_utf8(response.body).unwrap();
    assert!(text.contains("Amaterasu &amp; &lt;test&gt;"));
    assert!(text.contains("<appversion>7.1.0.0</appversion>"));
    assert!(text.contains("<ServerCodecModeSupport>0</ServerCodecModeSupport>"));
    assert!(text.contains("<MaxLumaPixelsHEVC>0</MaxLumaPixelsHEVC>"));
    assert!(text.contains("<PairStatus>0</PairStatus>"));
    assert!(text.contains("<currentgame>0</currentgame>"));
    assert!(text.contains("<StreamingSupported>0</StreamingSupported>"));
}

#[test]
fn hostname_updates_change_the_default_launch_device_name() {
    let mut router = streaming_router();
    router.set_hostname("Studio PC".into());
    let mut broker = Broker::default();
    let now = Instant::now();
    let target = concat!(
        "/launch?appid=7&mode=1280x720x60&",
        "rikey=00112233445566778899aabbccddeeff&rikeyid=17"
    );
    assert!(
        router
            .dispatch_with(
                19,
                request(target),
                peer("127.0.0.1", true, Some(vec![1, 2, 3]), true),
                now,
                &mut |command| broker.send(command),
            )
            .is_none()
    );

    let mut replies = Vec::new();
    for _ in 0..16 {
        replies.extend(poll(&mut router, &mut broker, now));
        if !replies.is_empty() {
            break;
        }
    }
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].1.status, 200);
    let rtsp_open = broker
        .accepted
        .iter()
        .find(|payload| payload["action"] == "open")
        .expect("RTSP open request");
    assert_eq!(rtsp_open["device_name"], "Studio PC");
}

#[test]
fn configured_streaming_advertises_only_the_supported_h264_codec() {
    let response = xml::server_info(
        &router().config,
        "host-id",
        true,
        0,
        true,
        true,
        EncoderSnapshot::default().advertisement(),
    );
    let text = String::from_utf8(response.body).unwrap();
    let expected_version = if cfg!(target_os = "linux") {
        "7.1.431.-1"
    } else {
        "7.1.431.0"
    };
    assert!(text.contains(&format!("<appversion>{expected_version}</appversion>")));
    assert!(text.contains("<StreamingSupported>1</StreamingSupported>"));
    assert!(text.contains("<currentgame>0</currentgame>"));
    assert!(text.contains("<state>AMATERASU_SERVER_BUSY</state>"));
    assert!(text.contains("<ServerCodecModeSupport>1</ServerCodecModeSupport>"));
    assert!(text.contains("<MaxLumaPixelsHEVC>0</MaxLumaPixelsHEVC>"));
}

#[test]
fn serverinfo_codec_mask_and_hevc_luma_follow_the_encoder_snapshot() {
    let encoder = |codec: &str, hardware: bool| {
        json!({"identity":{"clsid":format!("test.{codec}.{hardware}"),"hardware_url":null,
            "friendly_name":codec},"name":codec,"codec":codec,"hardware":hardware,
            "vendor_id":null})
    };
    let cases = [
        (vec![encoder("h264", false)], "1", "0"),
        (
            vec![encoder("h264", true), encoder("hevc", true)],
            "257",
            "1869449984",
        ),
        (
            vec![encoder("h264", true), encoder("av1", true)],
            "65537",
            "0",
        ),
        (
            vec![
                encoder("h264", true),
                encoder("hevc", true),
                encoder("av1", true),
            ],
            "65793",
            "1869449984",
        ),
    ];
    for (encoders, mask, luma) in cases {
        let snapshot =
            EncoderSnapshot::parse(&json!({"ok":true,"enabled":true,"encoders":encoders})).unwrap();
        let response = xml::server_info(
            &router().config,
            "host-id",
            true,
            0,
            false,
            true,
            snapshot.advertisement(),
        );
        let text = String::from_utf8(response.body).unwrap();
        assert!(
            text.contains(&format!(
                "<ServerCodecModeSupport>{mask}</ServerCodecModeSupport>"
            )),
            "{text}"
        );
        assert!(text.contains(&format!("<MaxLumaPixelsHEVC>{luma}</MaxLumaPixelsHEVC>")));
    }
}

#[test]
fn public_launch_requires_paired_tls_before_parsing_private_fields() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();
    let malformed = "/launch?appid=bad&mode=bad&rikey=bad&rikeyid=bad";
    let response = router
        .dispatch_with(
            1,
            request(malformed),
            peer("127.0.0.1", true, Some(vec![1, 2, 3]), false),
            now,
            &mut |command| broker.send(command),
        )
        .unwrap();
    assert_eq!(response.status, 401);
    assert!(broker.accepted.is_empty());

    let response = router
        .dispatch_with(
            2,
            request(malformed),
            peer("127.0.0.1", true, Some(vec![1, 2, 3]), true),
            now,
            &mut |command| broker.send(command),
        )
        .unwrap();
    assert_eq!(response.status, 400);
    assert!(broker.accepted.is_empty());
}

#[test]
fn launch_identity_and_client_override_come_from_the_authenticated_certificate() {
    let mut router = streaming_router();
    router
        .launches
        .set_audio_sink_settings(Some("endpoint.global"));
    router.launches.set_audio_sink_capture_only(true);
    let mut broker = Broker::default();
    let now = Instant::now();
    let target = concat!(
        "/launch?appid=7&mode=1280x720x60&",
        "rikey=00112233445566778899aabbccddeeff&rikeyid=17&uniqueid=forged-client",
        "&clientName=Moonlight+Desk&localAudioPlayMode=1"
    );
    assert!(
        router
            .dispatch_with(
                19,
                request(target),
                peer("127.0.0.1", true, Some(vec![1, 2, 3]), true),
                now,
                &mut |command| broker.send(command),
            )
            .is_none()
    );

    assert!(poll(&mut router, &mut broker, now).is_empty());
    let pairing_lookup = broker
        .accepted
        .iter()
        .find(|payload| payload["action"] == "check_client_certificate")
        .unwrap();
    assert_eq!(pairing_lookup["certificate_pem"], pem());
    assert!(pairing_lookup.get("uniqueid").is_none());

    let mut replies = Vec::new();
    for _ in 0..12 {
        replies.extend(poll(&mut router, &mut broker, now));
        if !replies.is_empty() {
            break;
        }
    }
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].0, 19);
    assert_eq!(replies[0].1.status, 200);

    let launch = broker
        .accepted
        .iter()
        .find(|payload| payload["action"] == "launch_export")
        .unwrap();
    assert_eq!(launch["app_id"], "app-0000000000000007");
    let rtsp_open = broker
        .accepted
        .iter()
        .find(|payload| payload["action"] == "open")
        .unwrap();
    // The pairing record's certificate-keyed value outranks the app value (37)
    // and global value (20). The query's forged uniqueid has no effect.
    assert_eq!(rtsp_open["profile"]["fec_percentage"], 49);
    assert_eq!(rtsp_open["stream_audio"], false);
    assert_eq!(rtsp_open["capture"]["audio_endpoint"], "endpoint.client");
    assert_eq!(rtsp_open["capture"]["host_audio_enabled"], true);
    assert_eq!(rtsp_open["capture_sink_without_routing"], true);
    assert_eq!(rtsp_open["device_name"], "Moonlight Desk");
    // The authenticated certificate-scoped explicit disable wins over the app.
    assert_eq!(rtsp_open["back_button_timeout"], -1);
    assert_eq!(rtsp_open["controller"], false);
    assert_eq!(rtsp_open["gamepad"], "auto");
    assert_eq!(rtsp_open["keyboard"], false);
    assert_eq!(rtsp_open["mouse"], true);
    assert_eq!(rtsp_open["ds4_back_as_touchpad_click"], false);
}

#[test]
fn server_certificate_request_is_held_until_pin_approval() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();
    assert!(dispatch(&mut router, &mut broker, 1, &begin_target(), now).is_none());
    for _ in 0..4 {
        assert!(poll(&mut router, &mut broker, now).is_empty());
    }
    assert_eq!(router.jobs.len(), 1);
    assert!(poll(&mut router, &mut broker, now + Duration::from_millis(100)).is_empty());
    broker.approved = true;
    let later = now + Duration::from_millis(300);
    assert!(poll(&mut router, &mut broker, later).is_empty());
    let replies = poll(&mut router, &mut broker, later);
    assert_eq!(replies.len(), 1);
    let text = String::from_utf8(replies[0].1.body.clone()).unwrap();
    assert!(text.contains("<paired>1</paired>"));
    assert!(text.contains("<plaincert>"));
    assert!(!text.contains("pending"));
}

#[test]
fn foreign_peer_duplicate_begin_and_unpair_cannot_consume_a_victim_transcript() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();
    ready(&mut router, &mut broker, now);
    let requests = broker.accepted.len();
    let challenge = format!(
        "/pair?uniqueid=client-1&clientchallenge={}",
        "00".repeat(16)
    );
    let foreign = router
        .dispatch_with(
            2,
            request(&challenge),
            peer("127.0.0.2", false, None, false),
            now,
            &mut |command| broker.send(command),
        )
        .unwrap();
    assert_eq!(foreign.status, 403);
    assert_eq!(
        dispatch(&mut router, &mut broker, 3, &begin_target(), now)
            .unwrap()
            .status,
        409
    );
    // Plaintext unpair is inert: it neither names a victim nor ends a transcript.
    let plaintext = dispatch(
        &mut router,
        &mut broker,
        4,
        "/unpair?uniqueid=client-1",
        now,
    )
    .unwrap();
    assert_eq!(plaintext.status, 200);
    assert!(
        String::from_utf8(plaintext.body)
            .unwrap()
            .contains("<unpaired>0</unpaired>")
    );
    assert_eq!(broker.accepted.len(), requests);
    assert_eq!(router.transcripts.len(), 1);
    assert!(dispatch(&mut router, &mut broker, 5, &challenge, now).is_none());
}

#[test]
fn duplicate_or_out_of_order_phases_never_replay_crypto_calls() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();
    ready(&mut router, &mut broker, now);
    let wrong = format!(
        "/pair?uniqueid=client-1&serverchallengeresp={}",
        "00".repeat(32)
    );
    assert_eq!(
        dispatch(&mut router, &mut broker, 2, &wrong, now)
            .unwrap()
            .status,
        409
    );
    let challenge = format!(
        "/pair?uniqueid=client-1&clientchallenge={}",
        "00".repeat(16)
    );
    assert!(dispatch(&mut router, &mut broker, 3, &challenge, now).is_none());
    assert_eq!(
        dispatch(&mut router, &mut broker, 4, &challenge, now)
            .unwrap()
            .status,
        409
    );
    assert!(poll(&mut router, &mut broker, now).is_empty());
    assert_eq!(poll(&mut router, &mut broker, now)[0].1.status, 200);
    assert_eq!(
        dispatch(&mut router, &mut broker, 5, &challenge, now)
            .unwrap()
            .status,
        409
    );
    assert_eq!(
        broker
            .accepted
            .iter()
            .filter(|payload| payload["action"] == "client_challenge")
            .count(),
        1
    );
}

#[test]
fn cancelled_begin_is_drained_then_cancelled_by_opaque_id() {
    let mut router = router();
    let mut broker = Broker {
        pause_begin: true,
        ..Default::default()
    };
    let now = Instant::now();
    dispatch(&mut router, &mut broker, 1, &begin_target(), now);
    poll(&mut router, &mut broker, now);
    router.cancel_with(1, now, &mut |command| broker.send(command));
    assert!(poll(&mut router, &mut broker, now).is_empty());
    broker.pause_begin = false;
    for _ in 0..3 {
        assert!(poll(&mut router, &mut broker, now).is_empty());
    }
    assert!(router.transcripts.is_empty());
    assert!(
        broker
            .accepted
            .iter()
            .any(|payload| payload["action"] == "cancel"
                && payload["pairing_id"] == "test-private-pairing-id")
    );
}

#[test]
fn timeout_closes_held_pin_request_and_cancels_the_transcript() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();
    dispatch(&mut router, &mut broker, 1, &begin_target(), now);
    for _ in 0..4 {
        poll(&mut router, &mut broker, now);
    }
    let expired = now + PAIRING_TIMEOUT;
    let replies = poll(&mut router, &mut broker, expired);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].1.status, 504);
    assert!(router.transcripts.is_empty());
    poll(&mut router, &mut broker, expired);
    poll(&mut router, &mut broker, expired);
    assert!(
        broker
            .accepted
            .iter()
            .any(|payload| payload["action"] == "cancel")
    );
}

#[test]
fn final_pairchallenge_requires_live_tls_trust_and_the_original_certificate() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();
    ready(&mut router, &mut broker, now);
    for (field, length) in [
        ("clientchallenge", 16),
        ("serverchallengeresp", 32),
        ("clientpairingsecret", 272),
    ] {
        let target = format!("/pair?uniqueid=client-1&{field}={}", "00".repeat(length));
        assert!(dispatch(&mut router, &mut broker, 2, &target, now).is_none());
        poll(&mut router, &mut broker, now);
        assert_eq!(poll(&mut router, &mut broker, now)[0].1.status, 200);
    }
    let target = "/pair?uniqueid=client-1&phrase=pairchallenge";
    assert_eq!(
        dispatch(&mut router, &mut broker, 3, target, now)
            .unwrap()
            .status,
        401
    );
    for (certificate, trusted, expected) in [
        (vec![1, 2, 3], false, 401),
        (vec![9], true, 403),
        (vec![1, 2, 3], true, 200),
    ] {
        let reply = router
            .dispatch_with(
                4,
                request(target),
                peer("127.0.0.1", true, Some(certificate), trusted),
                now,
                &mut |command| broker.send(command),
            )
            .unwrap();
        assert_eq!(reply.status, expected);
    }
}

fn peer(
    remote: &str,
    secure: bool,
    peer_certificate: Option<Vec<u8>>,
    peer_trusted: bool,
) -> RequestContext {
    RequestContext {
        remote: remote.parse().unwrap(),
        secure,
        peer_certificate,
        peer_trusted,
        list_allowed: peer_trusted,
        launch_allowed: peer_trusted,
        entitlement_allowed: false,
    }
}

#[test]
fn configured_hostname_replaces_xml_noncharacters_and_escapes_quotes() {
    let mut config = router().config;
    config.hostname = "A&<\"'\u{fffe}\u{ffff}Z".into();
    let reply = xml::server_info(
        &config,
        "host",
        false,
        0,
        false,
        false,
        CodecAdvertisement::NONE,
    );
    let body = String::from_utf8(reply.body).unwrap();
    assert!(body.contains("<hostname>A&amp;&lt;&quot;&apos;  Z</hostname>"));
    assert!(!body.contains('\u{fffe}'));
    assert!(!body.contains('\u{ffff}'));
}

#[test]
fn committed_hostname_updates_are_visible_in_live_serverinfo_without_restart() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();

    assert!(dispatch(&mut router, &mut broker, 1, "/serverinfo", now).is_none());
    assert!(poll(&mut router, &mut broker, now).is_empty());
    let initial = poll(&mut router, &mut broker, now);
    assert_eq!(initial.len(), 1);
    let initial_body = String::from_utf8(initial[0].1.body.clone()).unwrap();
    assert!(initial_body.contains("<hostname>Amaterasu &amp; &lt;test&gt;</hostname>"));

    router.set_hostname("Living Room & Games".into());
    router.set_ports(48_000, 47_995);
    assert!(dispatch(&mut router, &mut broker, 2, "/serverinfo", now).is_none());
    assert!(poll(&mut router, &mut broker, now).is_empty());
    let updated = poll(&mut router, &mut broker, now);
    assert_eq!(updated.len(), 1);
    let updated_body = String::from_utf8(updated[0].1.body.clone()).unwrap();
    assert!(updated_body.contains("<hostname>Living Room &amp; Games</hostname>"));
    assert!(updated_body.contains("<ExternalPort>48000</ExternalPort>"));
    assert!(updated_body.contains("<HttpsPort>47995</HttpsPort>"));
}

#[test]
fn cancellation_before_begin_dispatch_does_not_create_a_transcript() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();
    dispatch(&mut router, &mut broker, 1, &begin_target(), now);
    router.cancel_with(1, now, &mut |command| broker.send(command));
    assert!(poll(&mut router, &mut broker, now).is_empty());
    assert!(broker.accepted.is_empty());
    assert!(router.transcripts.is_empty());
}

#[test]
fn conflicting_phase_selectors_and_foreign_certificate_do_not_advance() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();
    ready(&mut router, &mut broker, now);
    let invalid = format!(
        "/pair?uniqueid=client-1&phrase=pairchallenge&clientchallenge={}",
        "00".repeat(16)
    );
    assert_eq!(
        dispatch(&mut router, &mut broker, 2, &invalid, now)
            .unwrap()
            .status,
        400
    );
    let replacement =
        pem_rfc7468::encode_string("CERTIFICATE", pem_rfc7468::LineEnding::LF, &[9, 9, 9]).unwrap();
    let duplicate = begin_target().replace(&hex(pem().as_bytes()), &hex(replacement.as_bytes()));
    assert_eq!(
        dispatch(&mut router, &mut broker, 3, &duplicate, now)
            .unwrap()
            .status,
        409
    );
    let phase = format!(
        "/pair?uniqueid=client-1&clientchallenge={}&clientcert={}",
        "00".repeat(16),
        hex(replacement.as_bytes())
    );
    assert_eq!(
        dispatch(&mut router, &mut broker, 4, &phase, now)
            .unwrap()
            .status,
        403
    );
    assert_eq!(
        broker
            .accepted
            .iter()
            .filter(
                |payload| payload["action"] == "client_challenge" || payload["action"] == "cancel"
            )
            .count(),
        0
    );
    assert_eq!(router.transcripts.len(), 1);
}

fn server_info_text(router: &mut Router, broker: &mut Broker, now: Instant) -> String {
    assert!(dispatch(router, broker, 31, "/serverinfo", now).is_none());
    for _ in 0..4 {
        let replies = poll(router, broker, now);
        if let Some((_, reply)) = replies.into_iter().find(|(id, _)| *id == 31) {
            assert_eq!(reply.status, 200);
            return String::from_utf8(reply.body).unwrap();
        }
    }
    panic!("serverinfo did not complete");
}

fn codec_fields(text: &str) -> (u32, u32) {
    let field = |name: &str| -> u32 {
        let open = format!("<{name}>");
        let start = text.find(&open).unwrap() + open.len();
        let end = text[start..].find('<').unwrap() + start;
        text[start..end].parse().unwrap()
    };
    (field("ServerCodecModeSupport"), field("MaxLumaPixelsHEVC"))
}

const SCM_H264: u32 = 0x1;
const SCM_HEVC: u32 = 0x100;

#[test]
fn serverinfo_without_streaming_never_asks_for_encoders() {
    let mut router = router();
    let mut broker = Broker::default();
    let text = server_info_text(&mut router, &mut broker, Instant::now());
    assert_eq!(codec_fields(&text), (0, 0));
    assert!(
        broker
            .requests
            .values()
            .all(|payload| payload["action"] != "enumerate")
    );
}

#[test]
fn serverinfo_reads_the_shared_encoder_snapshot_without_encoder_calls() {
    let mut router = streaming_router();
    let mut broker = Broker::default();
    let now = Instant::now();
    assert_eq!(
        codec_fields(&server_info_text(&mut router, &mut broker, now)),
        (SCM_H264, 0)
    );
    router.set_encoder_snapshot(
        EncoderSnapshot::parse(
            &json!({"ok":true,"enabled":true,"encoders":[{"codec":"hevc","hardware":true}]}),
        )
        .unwrap(),
    );
    assert_eq!(
        codec_fields(&server_info_text(&mut router, &mut broker, now)),
        (SCM_H264 | SCM_HEVC, 1_869_449_984)
    );
    assert!(
        broker
            .requests
            .values()
            .all(|payload| payload["action"] != "enumerate")
    );
}

const LAUNCH_TARGET: &str = concat!(
    "/launch?appid=7&mode=1280x720x60&",
    "rikey=00112233445566778899aabbccddeeff&rikeyid=17"
);

fn poll_until_reply(router: &mut Router, broker: &mut Broker, now: Instant) -> (u64, Reply) {
    for _ in 0..32 {
        if let Some(reply) = poll(router, broker, now).into_iter().next() {
            return reply;
        }
    }
    panic!("no reply was produced");
}

fn trusted_tls(certificate: &[u8]) -> RequestContext {
    peer("127.0.0.1", true, Some(certificate.to_vec()), true)
}

fn unpair_over_tls(
    router: &mut Router,
    broker: &mut Broker,
    connection: u64,
    now: Instant,
) -> Reply {
    assert!(
        router
            .dispatch_with(
                connection,
                request("/unpair?uniqueid=ignored-metadata"),
                trusted_tls(&[1, 2, 3]),
                now,
                &mut |command| broker.send(command),
            )
            .is_none()
    );
    let (id, reply) = poll_until_reply(router, broker, now);
    assert_eq!(id, connection);
    reply
}

#[test]
fn plaintext_unpair_answers_unpaired_zero_and_calls_no_service() {
    let mut router = streaming_router();
    let mut broker = Broker::default();
    let now = Instant::now();
    for target in ["/unpair", "/unpair?uniqueid=victim&uuid=1234"] {
        let reply = dispatch(&mut router, &mut broker, 1, target, now).unwrap();
        assert_eq!(reply.status, 200);
        assert!(!reply.requires_paired_tls);
        let body = String::from_utf8(reply.body).unwrap();
        assert!(body.contains("<unpaired>0</unpaired>"), "{body}");
    }
    assert!(broker.accepted.is_empty());
}

#[test]
fn untrusted_tls_unpair_is_rejected_without_a_service_call() {
    let mut router = router();
    let mut broker = Broker::default();
    let reply = router
        .dispatch_with(
            1,
            request("/unpair"),
            peer("127.0.0.1", true, Some(vec![9, 9, 9]), false),
            Instant::now(),
            &mut |command| broker.send(command),
        )
        .unwrap();
    assert_eq!(reply.status, 401);
    assert!(broker.accepted.is_empty());
}

#[test]
fn paired_tls_unpair_removes_the_presented_certificate_and_answers_one() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();
    let reply = unpair_over_tls(&mut router, &mut broker, 5, now);
    assert_eq!(reply.status, 200);
    // The certificate stops being trusted the moment the removal commits; the
    // acknowledgement must still reach the client that asked for it.
    assert!(!reply.requires_paired_tls);
    let delivered = reply.authorize(false);
    assert_eq!(delivered.status, 200);
    let body = String::from_utf8(delivered.body).unwrap();
    assert!(body.contains("<unpaired>1</unpaired>"), "{body}");

    let removal = broker
        .accepted
        .iter()
        .find(|payload| payload["action"] == "unpair_presented_client")
        .unwrap();
    assert_eq!(removal["certificate_pem"], pem());
    assert!(removal.get("uniqueid").is_none());
    assert!(removal.get("certificate_id").is_none());
}

#[test]
fn paired_tls_unpair_reports_zero_when_nothing_was_removed() {
    let mut router = router();
    let mut broker = Broker {
        unpair_misses: true,
        ..Broker::default()
    };
    let reply = unpair_over_tls(&mut router, &mut broker, 5, Instant::now());
    assert_eq!(reply.status, 200);
    let body = String::from_utf8(reply.body).unwrap();
    assert!(body.contains("<unpaired>0</unpaired>"), "{body}");
}

#[test]
fn unpair_rejects_unknown_query_fields_before_calling_the_pairing_service() {
    let mut router = router();
    let mut broker = Broker::default();
    let reply = router
        .dispatch_with(
            1,
            request("/unpair?certificate_id=00"),
            trusted_tls(&[1, 2, 3]),
            Instant::now(),
            &mut |command| broker.send(command),
        )
        .unwrap();
    assert_eq!(reply.status, 400);
    assert!(broker.accepted.is_empty());
}

#[test]
fn resume_requires_complete_launch_metadata_and_paired_tls() {
    let mut router = router();
    let mut broker = Broker::default();
    let trusted = router
        .dispatch_with(
            1,
            request("/resume?uniqueid=client"),
            trusted_tls(&[1, 2, 3]),
            Instant::now(),
            &mut |command| broker.send(command),
        )
        .unwrap();
    assert_eq!(trusted.status, 400);
    let plaintext = dispatch(&mut router, &mut broker, 2, "/resume", Instant::now()).unwrap();
    assert_eq!(plaintext.status, 401);
    assert!(broker.accepted.is_empty());
}

#[test]
fn unpairing_a_client_cancels_its_active_launch_through_the_revoke_path() {
    let mut router = streaming_router();
    let mut broker = Broker::default();
    let now = Instant::now();
    assert!(
        router
            .dispatch_with(
                19,
                request(LAUNCH_TARGET),
                trusted_tls(&[1, 2, 3]),
                now,
                &mut |command| broker.send(command),
            )
            .is_none()
    );
    let (_, launched) = poll_until_reply(&mut router, &mut broker, now);
    assert_eq!(launched.status, 200);
    router.reply_delivered(19);
    let owner = PeerIdentity::new("127.0.0.1".parse().unwrap(), vec![1, 2, 3]).unwrap();
    assert_eq!(router.launches.current_game(&owner), 7);
    assert!(router.launches.busy());

    let reply = unpair_over_tls(&mut router, &mut broker, 20, now);
    assert!(
        String::from_utf8(reply.body)
            .unwrap()
            .contains("<unpaired>1</unpaired>")
    );

    // Mirrors NvhttpModule::tick: after the pairing authority drops the
    // certificate, every non-terminal launch it owns is revoked.
    let unrelated = [9u8, 9, 9];
    router.revoke_untrusted(|certificate| certificate == unrelated);
    let actions = |broker: &Broker| {
        broker
            .accepted
            .iter()
            .filter_map(|payload| payload["action"].as_str().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    for _ in 0..64 {
        poll(&mut router, &mut broker, now);
        if !router.launches.busy() {
            break;
        }
    }
    let actions = actions(&broker);
    assert!(actions.contains(&"close".to_owned()), "{actions:?}");
    assert!(actions.contains(&"stop".to_owned()), "{actions:?}");
    assert!(!router.launches.busy());
    assert_eq!(router.launches.current_game(&owner), 0);
}

#[test]
fn launch_permission_loss_stops_owned_session_while_certificate_remains_paired() {
    let mut router = streaming_router();
    let mut broker = Broker::default();
    let now = Instant::now();
    assert!(
        router
            .dispatch_with(
                19,
                request(LAUNCH_TARGET),
                trusted_tls(&[1, 2, 3]),
                now,
                &mut |command| broker.send(command)
            )
            .is_none()
    );
    let (_, launched) = poll_until_reply(&mut router, &mut broker, now);
    assert_eq!(launched.status, 200);
    router.reply_delivered(19);
    let certificate = [1, 2, 3];
    let paired = |peer: &[u8]| peer == certificate;
    assert!(paired(&certificate));
    router.revoke_untrusted(|peer| paired(peer) && peer != certificate);
    for _ in 0..64 {
        poll(&mut router, &mut broker, now);
        if !router.launches.busy() {
            break;
        }
    }
    assert!(!router.launches.busy());
    assert!(paired(&certificate));
    assert!(
        broker
            .accepted
            .iter()
            .any(|payload| payload["action"] == "stop")
    );
}

#[test]
fn untrusted_tls_serverinfo_is_an_http_success_carrying_xml_401() {
    let mut router = router();
    let mut broker = Broker::default();
    let reply = router
        .dispatch_with(
            1,
            request("/serverinfo?uniqueid=client"),
            peer("127.0.0.1", true, Some(vec![9, 9, 9]), false),
            Instant::now(),
            &mut |command| broker.send(command),
        )
        .unwrap();
    // A non-2xx HTTP status makes moonlight-qt report the host offline instead
    // of falling back to plain-HTTP /serverinfo; the XML status drives that.
    assert_eq!(reply.status, 200);
    let body = String::from_utf8(reply.body).unwrap();
    assert!(body.contains("status_code=\"401\""), "{body}");
    assert!(!body.contains("PairStatus"));
    assert!(broker.accepted.is_empty());
}

#[test]
fn denied_launch_permission_blocks_known_ids_and_resume_before_work() {
    let mut router = streaming_router();
    let mut broker = Broker::default();
    let now = Instant::now();
    for target in ["/launch?appid=7", "/resume?appid=7"] {
        let mut context = trusted_tls(&[1, 2, 3]);
        context.launch_allowed = false;
        let reply = router
            .dispatch_with(1, request(target), context, now, &mut |command| {
                broker.send(command)
            })
            .unwrap();
        assert_eq!(reply.status, 403);
    }
    assert!(broker.accepted.is_empty());
}

fn neutral_binding(method: &str, api: &str) -> participants::Binding {
    participants::Binding {
        path: "/neutral".into(),
        http_method: method.into(),
        service: format!("org.example.{api}/1"),
        method: "handle".into(),
        interface_major: 1,
        interface_minor: 0,
        schema: "org.example.request/1".into(),
        upgrade: None,
        entitlement: "org.example/read".into(),
    }
}

fn neutral_request(method: &str, body: &[u8]) -> Request {
    Request {
        method: method.into(),
        target: "/neutral?arg=value".into(),
        body: body.into(),
    }
}

fn entitled_peer() -> RequestContext {
    let mut peer = trusted_tls(b"certificate");
    peer.entitlement_allowed = true;
    peer
}

#[test]
fn approved_participants_use_exact_api_and_validate_responses() {
    let now = Instant::now();
    let mut router = router();
    router.set_participants(vec![
        neutral_binding("GET", "first"),
        neutral_binding("POST", "second"),
    ]);
    let mut accepted = Vec::new();
    let mut send = |command| {
        Ok(match command {
            Command::CallApi {
                service, payload, ..
            } => {
                accepted.push((service, payload));
                Response::Accepted {
                    id: accepted.len() as u64,
                }
            }
            Command::TakeResult { request } => Response::Result {
                payload: if request == 1 {
                    json!({"version":1,"status":200,"content_type":"text/plain","body_base64":"b2s="})
                } else {
                    json!({"version":1,"status":200,"content_type":"text/plain\r\nInjected: x","body_base64":"b2s="})
                },
            },
            _ => panic!("unexpected broker command"),
        })
    };
    assert!(
        router
            .dispatch_with(
                1,
                neutral_request("GET", b""),
                entitled_peer(),
                now,
                &mut send
            )
            .is_none()
    );
    assert!(
        router
            .dispatch_with(
                2,
                neutral_request("POST", b"test"),
                entitled_peer(),
                now,
                &mut send
            )
            .is_none()
    );
    assert!(
        router
            .poll_with_identity(now, &mut send, |_, _| true)
            .is_empty()
    );
    let replies = router.poll_with_identity(now, &mut send, |_, _| true);
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0].1.status, 200);
    assert_eq!(replies[0].1.body, b"ok");
    assert_eq!(replies[1].1.status, 502);
    drop(send);
    assert_eq!(accepted[0].0, "org.example.first/1");
    assert_eq!(accepted[1].0, "org.example.second/1");
}

#[test]
fn participant_denial_bounds_and_disconnect_are_enforced() {
    let now = Instant::now();
    let mut router = router();
    router.set_participants(vec![neutral_binding("POST", "first")]);
    let mut commands = Vec::new();
    let mut send = |command| {
        commands.push(format!("{command:?}"));
        Ok(match command {
            Command::CallApi { .. } => Response::Accepted { id: 7 },
            Command::Cancel { .. } => Response::Ok,
            Command::TakeResult { .. } => Response::Error {
                code: "cancelled".into(),
                message: String::new(),
            },
            _ => panic!("unexpected broker command"),
        })
    };
    let mut denied = entitled_peer();
    denied.entitlement_allowed = false;
    assert_eq!(
        router
            .dispatch_with(1, neutral_request("POST", b""), denied, now, &mut send)
            .unwrap()
            .status,
        403
    );
    assert_eq!(
        router
            .dispatch_with(
                2,
                neutral_request("POST", &vec![0; protocol_handler::MAX_BODY_BYTES + 1]),
                entitled_peer(),
                now,
                &mut send
            )
            .unwrap()
            .status,
        413
    );
    assert!(
        router
            .dispatch_with(
                3,
                neutral_request("POST", b"ok"),
                entitled_peer(),
                now,
                &mut send
            )
            .is_none()
    );
    assert!(
        router
            .poll_with_identity(now, &mut send, |_, _| true)
            .is_empty()
    );
    router.cancel_with(3, now, &mut send);
    assert!(
        router
            .poll_with_identity(now, &mut send, |_, _| true)
            .is_empty()
    );
    drop(send);
    assert!(commands.iter().any(|command| command.contains("Cancel")));
}

#[test]
fn participant_revocation_cancels_pending_work() {
    let now = Instant::now();
    let mut router = router();
    router.set_participants(vec![neutral_binding("POST", "first")]);
    let mut cancelled = false;
    let mut send = |command| {
        Ok(match command {
            Command::CallApi { .. } => Response::Accepted { id: 7 },
            Command::Cancel { .. } => {
                cancelled = true;
                Response::Ok
            }
            Command::TakeResult { .. } => Response::Error {
                code: "cancelled".into(),
                message: String::new(),
            },
            _ => panic!("unexpected broker command"),
        })
    };
    assert!(
        router
            .dispatch_with(
                1,
                neutral_request("POST", b"ok"),
                entitled_peer(),
                now,
                &mut send
            )
            .is_none()
    );
    assert!(
        router
            .poll_with_identity(now, &mut send, |_, _| true)
            .is_empty()
    );
    let replies = router.poll_with_identity(now, &mut send, |_, _| false);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].1.status, 504);
    drop(send);
    assert!(cancelled);
}

#[test]
fn approved_broker_can_replace_participant_without_changing_consumer() {
    use amaterasu_contracts::{
        ApiFieldKind, ApiMethod, ApiMethodApproval, ApiPrivilege, ApiSchema, Grants, RuntimeConfig,
    };
    use amaterasu_runtime::Broker;
    let schema = |required: &[(&str, ApiFieldKind)]| ApiSchema {
        required: required
            .iter()
            .map(|(name, kind)| ((*name).into(), kind.clone()))
            .collect(),
        optional: BTreeMap::new(),
        allow_extra: false,
    };
    let request_schema = schema(&[
        ("version", ApiFieldKind::Integer),
        ("schema", ApiFieldKind::String),
        ("method", ApiFieldKind::String),
        ("target", ApiFieldKind::String),
        ("body_base64", ApiFieldKind::String),
        ("peer", ApiFieldKind::Object),
    ]);
    let response_schema = schema(&[
        ("version", ApiFieldKind::Integer),
        ("status", ApiFieldKind::Integer),
        ("content_type", ApiFieldKind::String),
        ("body_base64", ApiFieldKind::String),
    ]);
    let binding = neutral_binding("POST", "first");
    let method = ApiMethod {
        service: binding.service.clone(),
        method: binding.method.clone(),
        interface_major: 1,
        interface_minor: 0,
        request: request_schema,
        response: response_schema,
        privilege: ApiPrivilege::User,
    };
    for provider in [
        "org.example.first-provider",
        "org.example.replacement-provider",
    ] {
        let mut config = RuntimeConfig::default();
        config.principals.insert(
            "amaterasu.nvhttp".into(),
            Grants {
                call: [method.service.clone()].into(),
                ..Default::default()
            },
        );
        config.principals.insert(
            provider.into(),
            Grants {
                provide: [method.service.clone()].into(),
                ..Default::default()
            },
        );
        config.api_methods.push(ApiMethodApproval {
            provider: provider.into(),
            callers: ["amaterasu.nvhttp".into()].into(),
            method: method.clone(),
        });
        let mut broker = Broker::new(config).unwrap();
        assert!(matches!(
            broker.dispatch(
                provider,
                Command::RegisterApi {
                    method: method.clone()
                }
            ),
            Response::Ok
        ));
        let now = Instant::now();
        let mut router = router();
        router.set_participants(vec![binding.clone()]);
        let mut owner = |command| Ok(broker.dispatch("amaterasu.nvhttp", command));
        assert!(
            router
                .dispatch_with(
                    1,
                    neutral_request("POST", b"ok"),
                    entitled_peer(),
                    now,
                    &mut owner
                )
                .is_none()
        );
        assert!(
            router
                .poll_with_identity(now, &mut owner, |_, _| true)
                .is_empty()
        );
        let Response::Request {
            id,
            caller,
            payload,
            ..
        } = broker.dispatch(provider, Command::Poll {})
        else {
            panic!("provider did not receive selected API request")
        };
        assert_eq!(caller, "amaterasu.nvhttp");
        assert_eq!(payload["method"], "handle");
        assert!(matches!(broker.dispatch(provider, Command::Complete { request: id, payload: json!({"version":1,"status":200,"content_type":"text/plain","body_base64":"b2s="}) }), Response::Ok));
        let replies = router.poll_with_identity(
            now,
            &mut |command| Ok(broker.dispatch("amaterasu.nvhttp", command)),
            |_, _| true,
        );
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].1.body, b"ok");
    }
}

#[test]
fn lost_admission_retries_only_while_authorized_with_same_identity() {
    use std::cell::RefCell;
    let now = Instant::now();
    let mut router = router();
    router.set_participants(vec![neutral_binding("POST", "first")]);
    let attempts = RefCell::new(Vec::new());
    let mut send = |command: Command| match command {
        Command::CallApi {
            operation_id,
            payload,
            timeout_ms,
            ..
        } => {
            let mut calls = attempts.borrow_mut();
            calls.push((operation_id, payload, timeout_ms));
            if calls.len() == 1 {
                Err("admission reply lost".into())
            } else {
                Ok(Response::Accepted { id: 7 })
            }
        }
        Command::Cancel { .. } => Ok(Response::Ok),
        Command::TakeResult { .. } => Ok(Response::Error {
            code: "cancelled".into(),
            message: String::new(),
        }),
        _ => panic!("unexpected broker command"),
    };
    assert!(
        router
            .dispatch_with(
                1,
                neutral_request("POST", b"ok"),
                entitled_peer(),
                now,
                &mut send
            )
            .is_none()
    );
    assert!(
        router
            .poll_with_identity(now, &mut send, |_, _| true)
            .is_empty()
    );
    assert_eq!(attempts.borrow().len(), 1);
    assert!(
        router
            .poll_with_identity(now + Duration::from_millis(99), &mut send, |_, _| true)
            .is_empty()
    );
    assert_eq!(attempts.borrow().len(), 1);
    assert!(
        router
            .poll_with_identity(now + Duration::from_millis(100), &mut send, |_, _| true)
            .is_empty()
    );
    let calls = attempts.borrow();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, calls[1].0);
    assert_eq!(calls[0].1, calls[1].1);
    assert!(calls[1].2 < calls[0].2);
    drop(calls);
    router.cancel_with(1, now + Duration::from_millis(101), &mut send);
}

#[test]
fn lost_admission_is_never_retried_after_disconnect_or_revocation() {
    use std::cell::Cell;
    for disconnect in [true, false] {
        let now = Instant::now();
        let mut router = router();
        router.set_participants(vec![neutral_binding("POST", "first")]);
        let attempts = Cell::new(0);
        let mut send = |command: Command| {
            if matches!(command, Command::CallApi { .. }) {
                attempts.set(attempts.get() + 1);
                Err("admission reply lost".into())
            } else {
                panic!("unknown admission must not be retried or cancelled using a guessed ID")
            }
        };
        assert!(
            router
                .dispatch_with(
                    1,
                    neutral_request("POST", b"ok"),
                    entitled_peer(),
                    now,
                    &mut send
                )
                .is_none()
        );
        assert!(
            router
                .poll_with_identity(now, &mut send, |_, _| true)
                .is_empty()
        );
        if disconnect {
            router.cancel_with(1, now + Duration::from_millis(1), &mut send);
        } else {
            assert_eq!(
                router
                    .poll_with_identity(now + Duration::from_millis(1), &mut send, |_, _| false)
                    .len(),
                1
            );
        }
        assert!(
            router
                .poll_with_identity(now + Duration::from_millis(200), &mut send, |_, _| true)
                .is_empty()
        );
        assert_eq!(attempts.get(), 1);
        assert_eq!(router.jobs.len(), 1);
        assert!(
            router
                .poll_with_identity(now + SERVICE_TIMEOUT, &mut send, |_, _| true)
                .is_empty()
        );
        assert!(router.jobs.is_empty());
    }
}

#[test]
fn participant_shutdown_cancels_known_work_without_retrying_unknown_admission() {
    use std::cell::RefCell;
    let now = Instant::now();
    let mut router = router();
    router.set_participants(vec![
        neutral_binding("POST", "first"),
        neutral_binding("GET", "second"),
    ]);
    let calls = RefCell::new(Vec::new());
    let mut send = |command: Command| {
        calls.borrow_mut().push(format!("{command:?}"));
        match command {
            Command::CallApi { service, .. } if service == "org.example.first/1" => {
                Ok(Response::Accepted { id: 7 })
            }
            Command::CallApi { .. } => Err("admission reply lost".into()),
            Command::Cancel { request: 7 } => Ok(Response::Ok),
            Command::TakeResult { request: 7 } => Ok(Response::Error {
                code: "cancelled".into(),
                message: String::new(),
            }),
            _ => panic!("unexpected broker command"),
        }
    };
    assert!(
        router
            .dispatch_with(
                1,
                neutral_request("POST", b""),
                entitled_peer(),
                now,
                &mut send
            )
            .is_none()
    );
    assert!(
        router
            .dispatch_with(
                2,
                neutral_request("GET", b""),
                entitled_peer(),
                now,
                &mut send
            )
            .is_none()
    );
    assert!(
        router
            .poll_with_identity(now, &mut send, |_, _| true)
            .is_empty()
    );
    router.shutdown_with(now, &mut send);
    let calls = calls.borrow();
    assert_eq!(
        calls.iter().filter(|call| call.contains("CallApi")).count(),
        2
    );
    assert!(
        calls
            .iter()
            .any(|call| call.contains("Cancel { request: 7 }"))
    );
    assert!(router.jobs.is_empty());
}

#[test]
fn approved_handler_obeys_exact_wrapped_broker_size_limit() {
    use amaterasu_contracts::{
        ApiFieldKind, ApiMethod, ApiMethodApproval, ApiPrivilege, ApiSchema, Grants, RuntimeConfig,
    };
    use amaterasu_runtime::Broker;
    let schema = |required: &[(&str, ApiFieldKind)]| ApiSchema {
        required: required
            .iter()
            .map(|(name, kind)| ((*name).into(), kind.clone()))
            .collect(),
        optional: BTreeMap::new(),
        allow_extra: false,
    };
    let mut binding = neutral_binding("POST", "first");
    // Exercise method length and escaping in the actual broker wrapper.
    binding.method = format!("handler{}", "x".repeat(90));
    let method = ApiMethod {
        service: binding.service.clone(),
        method: binding.method.clone(),
        interface_major: 1,
        interface_minor: 0,
        request: schema(&[
            ("version", ApiFieldKind::Integer),
            ("schema", ApiFieldKind::String),
            ("method", ApiFieldKind::String),
            ("target", ApiFieldKind::String),
            ("body_base64", ApiFieldKind::String),
            ("peer", ApiFieldKind::Object),
        ]),
        response: schema(&[
            ("version", ApiFieldKind::Integer),
            ("status", ApiFieldKind::Integer),
            ("content_type", ApiFieldKind::String),
            ("body_base64", ApiFieldKind::String),
        ]),
        privilege: ApiPrivilege::User,
    };
    let provider = "org.example.provider";
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        "amaterasu.nvhttp".into(),
        Grants {
            call: [method.service.clone()].into(),
            ..Default::default()
        },
    );
    config.principals.insert(
        provider.into(),
        Grants {
            provide: [method.service.clone()].into(),
            ..Default::default()
        },
    );
    config.api_methods.push(ApiMethodApproval {
        provider: provider.into(),
        callers: ["amaterasu.nvhttp".into()].into(),
        method: method.clone(),
    });
    let mut broker = Broker::new(config).unwrap();
    assert!(matches!(
        broker.dispatch(
            provider,
            Command::RegisterApi {
                method: method.clone()
            }
        ),
        Response::Ok
    ));
    let certificate = vec![1; 7_800];
    let body = vec![2; protocol_handler::MAX_BODY_BYTES];
    let mut target = "/neutral?value=\"".to_owned();
    let wrapped_size = |target: &str| {
        let request = protocol_handler::Request {
            version: 1,
            schema: binding.schema.clone(),
            method: "POST".into(),
            target: target.into(),
            body_base64: STANDARD.encode(&body),
            peer: protocol_handler::VerifiedPeer {
                remote: "127.0.0.1".parse().unwrap(),
                certificate_der_base64: STANDARD.encode(&certificate),
            },
        };
        assert!(request.validate());
        serde_json::to_vec(&json!({"method": binding.method, "interface_major": 1, "interface_minor": 0, "arguments": request})).unwrap().len()
    };
    while wrapped_size(&target) < protocol_handler::MAX_MESSAGE_BYTES {
        target.push('x');
    }
    assert_eq!(wrapped_size(&target), protocol_handler::MAX_MESSAGE_BYTES);
    assert_eq!(
        wrapped_size(&format!("{target}x")),
        protocol_handler::MAX_MESSAGE_BYTES + 1
    );
    let now = Instant::now();
    let mut router = router();
    router.set_participants(vec![binding]);
    let make_peer = || {
        let mut peer = trusted_tls(&certificate);
        peer.entitlement_allowed = true;
        peer
    };
    let request = |target: String| Request {
        method: "POST".into(),
        target,
        body: body.clone(),
    };
    let mut owner = |command| Ok(broker.dispatch("amaterasu.nvhttp", command));
    assert!(
        router
            .dispatch_with(1, request(target.clone()), make_peer(), now, &mut owner)
            .is_none()
    );
    assert!(
        router
            .poll_with_identity(now, &mut owner, |_, _| true)
            .is_empty()
    );
    assert!(matches!(
        broker.dispatch(provider, Command::Poll {}),
        Response::Request { .. }
    ));
    target.push('x');
    let rejected = router
        .dispatch_with(2, request(target), make_peer(), now, &mut |command| {
            Ok(broker.dispatch("amaterasu.nvhttp", command))
        })
        .unwrap();
    assert_eq!(rejected.status, 413);
    assert!(matches!(
        broker.dispatch(provider, Command::Poll {}),
        Response::Pending
    ));
}

#[test]
fn configured_upgrade_requires_distinct_acceptance_and_fails_closed() {
    use amaterasu_module_sdk::data_plane::{self, DuplexPair, Hub, abi::*};
    use base64::{Engine, engine::general_purpose::STANDARD};
    let schema = Schema {
        namespace: 1,
        value: 1,
        version: 1,
    };
    let limits = Limits {
        max_chunk_bytes: 4,
        max_queued_bytes: 4,
        max_record_bytes: 4,
        max_queued_chunks: 1,
        max_segments: 1,
        max_metadata_bytes: 0,
        reserved: 0,
    };
    let input = data_plane::description(SOURCE, schema, schema, limits).unwrap();
    let output_schema = Schema { value: 2, ..schema };
    let output = data_plane::description(SINK, output_schema, schema, limits).unwrap();
    let hub = Hub::new(2, 8).unwrap();
    let pair = DuplexPair::create(&hub, "consumer", input, output).unwrap();
    let mut binding = neutral_binding("GET", "first");
    binding.upgrade = Some(participants::UpgradeProfile {
        protocol: "neutral".into(),
        input_format: schema,
        output_format: output_schema,
        metadata_schema: schema,
    });
    let offer = protocol_handler::UpgradeOffer {
        version: 1,
        request: protocol_handler::Request {
            version: 1,
            schema: binding.schema.clone(),
            method: "GET".into(),
            target: "/neutral".into(),
            body_base64: String::new(),
            peer: protocol_handler::VerifiedPeer {
                remote: "127.0.0.1".parse().unwrap(),
                certificate_der_base64: STANDARD.encode(b"certificate"),
            },
        },
        transfer_service: crate::DUPLEX_TRANSFER_SERVICE.into(),
        input_id: pair.input_id(),
        input,
        output_id: pair.output_id(),
        output,
    };
    for (decision, expected_reply, accepted) in [
        (json!({"outcome":"accept", "version":1}), None, true),
        (json!({"outcome":"reject", "version":1}), Some(403), false),
        (json!({"outcome":"accept", "version":2}), Some(502), false),
        (
            json!({"version":1,"status":200,"content_type":"text/plain","body_base64":""}),
            Some(502),
            false,
        ),
        (
            json!({"outcome":"accept", "version":1,"extra":true}),
            Some(502),
            false,
        ),
    ] {
        let mut router = router();
        router
            .queue_upgrade(1, binding.clone(), offer.clone(), b"certificate".to_vec())
            .unwrap();
        let now = Instant::now();
        let mut send = |command: Command| {
            Ok(match command {
                Command::CallApi { payload, .. } => {
                    assert_eq!(
                        serde_json::from_value::<protocol_handler::UpgradeOffer>(payload).unwrap(),
                        offer
                    );
                    Response::Accepted { id: 7 }
                }
                Command::TakeResult { request: 7 } => Response::Result {
                    payload: decision.clone(),
                },
                other => panic!("unexpected command: {other:?}"),
            })
        };
        assert!(
            router
                .poll_with_identity(now, &mut send, |_, _| true)
                .is_empty()
        );
        let replies = router.poll_with_identity(now, &mut send, |_, _| true);
        assert_eq!(
            replies.first().map(|(_, reply)| reply.status),
            expected_reply
        );
        assert_eq!(router.take_approved_upgrades().contains(&1), accepted);
    }
    let mut router = router();
    assert!(
        router
            .queue_upgrade(1, binding.clone(), offer.clone(), b"wrong".to_vec())
            .is_err()
    );
    binding.upgrade = None;
    assert!(
        router
            .queue_upgrade(1, binding, offer, b"certificate".to_vec())
            .is_err()
    );
}

fn neutral_launch_binding(field: &str, namespace: &str) -> launch_participants::Binding {
    use amaterasu_module_sdk::extension_api::SchemaRef;
    let schema = SchemaRef {
        id: "org.example.option".into(),
        major: 1,
        minor: 0,
        digest: "e020a99a8ed4ec086459efba9b8f0c73420143d452a894200c54b112560cc84d".into(),
    };
    launch_participants::Binding {
        service: format!("org.example.{field}/1"), method: "project".into(),
        interface_major: 1, interface_minor: 0, entitlement: "org.example/use".into(),
        fields: vec![field.into()], namespace: namespace.into(), version: 1,
        output_schema: schema.clone(), consumer_service: "amaterasu.rtsp/1".into(),
        consumer_schema: schema,
        schema_document: r#"{"$id":"org.example.option","$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"enabled":{"type":"boolean"}},"required":["enabled"],"additionalProperties":false}"#.into(),
    }
}

fn fixture_option_capabilities() -> Value {
    let schema = neutral_launch_binding("alpha", "org.example.alpha").consumer_schema;
    json!({"version":1,"options":[{"namespace":"org.example.alpha","version":1,"schema":schema},{"namespace":"org.example.beta","version":1,"schema":schema}]})
}

#[test]
fn launch_participant_bindings_fail_closed_for_conflict_and_schema_mismatch() {
    let alpha = neutral_launch_binding("alpha", "org.example.alpha");
    let beta = neutral_launch_binding("beta", "org.example.beta");
    assert!(launch_participants::validate(&[alpha.clone(), beta.clone()]).is_ok());
    let mut bad = beta.clone();
    bad.consumer_schema.digest = "0".repeat(64);
    assert!(launch_participants::validate(&[bad]).is_err());
    let mut bad = beta.clone();
    bad.fields = vec!["rikey".into()];
    assert!(launch_participants::validate(&[bad]).is_err());
    let mut bad = beta;
    bad.namespace = alpha.namespace.clone();
    bad.version = 2;
    assert!(launch_participants::validate(&[alpha.clone(), bad]).is_err());
    assert!(
        alpha
            .validate_reply(
                json!({"namespace":"org.example.beta", "version":1, "value":{"enabled":true}})
            )
            .is_err()
    );
    assert!(
        alpha
            .validate_reply(
                json!({"namespace":"org.example.alpha", "version":2, "value":{"enabled":true}})
            )
            .is_err()
    );
    assert!(
        alpha
            .validate_reply(
                json!({"namespace":"org.example.alpha", "version":1, "value":{"unknown":true}})
            )
            .is_err()
    );
    assert!(alpha.validate_reply(json!({"namespace":"org.example.alpha", "version":1, "value":{"enabled":true,"unknown":true}})).is_err());
}

#[test]
fn approved_launch_participants_reach_the_unchanged_stream_consumer_via_real_broker() {
    use amaterasu_contracts::{
        ApiFieldKind, ApiMethod, ApiMethodApproval, ApiPrivilege, ApiSchema, Grants, RuntimeConfig,
    };
    use amaterasu_runtime::Broker as RuntimeBroker;
    let shape = |fields: &[(&str, ApiFieldKind)]| ApiSchema {
        required: fields
            .iter()
            .map(|(name, kind)| ((*name).into(), kind.clone()))
            .collect(),
        optional: BTreeMap::new(),
        allow_extra: false,
    };
    let bindings = vec![
        neutral_launch_binding("alpha", "org.example.alpha"),
        neutral_launch_binding("beta", "org.example.beta"),
    ];
    assert!(launch_participants::validate(&bindings).is_ok());
    for provider in ["org.example.provider-one", "org.example.provider-two"] {
        let mut config = RuntimeConfig::default();
        let mut services: BTreeSet<String> = bindings
            .iter()
            .map(|binding| binding.service.clone())
            .collect();
        services.insert("amaterasu.rtsp/1".into());
        config.principals.insert(
            "amaterasu.nvhttp".into(),
            Grants {
                call: services.clone(),
                ..Default::default()
            },
        );
        config.principals.insert(
            provider.into(),
            Grants {
                provide: services,
                ..Default::default()
            },
        );
        let methods: Vec<_> = bindings
            .iter()
            .map(|binding| ApiMethod {
                service: binding.service.clone(),
                method: binding.method.clone(),
                interface_major: 1,
                interface_minor: 0,
                request: shape(&[
                    ("version", ApiFieldKind::Integer),
                    ("resume", ApiFieldKind::Boolean),
                    ("fields", ApiFieldKind::Object),
                ]),
                response: shape(&[
                    ("namespace", ApiFieldKind::String),
                    ("version", ApiFieldKind::Integer),
                    ("value", ApiFieldKind::Object),
                ]),
                privilege: ApiPrivilege::User,
            })
            .collect();
        for method in &methods {
            config.api_methods.push(ApiMethodApproval {
                provider: provider.into(),
                callers: ["amaterasu.nvhttp".into()].into(),
                method: method.clone(),
            });
        }
        let mut real = RuntimeBroker::new(config).unwrap();
        for method in methods {
            assert!(matches!(
                real.dispatch(provider, Command::RegisterApi { method }),
                Response::Ok
            ));
        }
        assert!(matches!(
            real.dispatch(
                provider,
                Command::Register {
                    service: "amaterasu.rtsp/1".into()
                }
            ),
            Response::Ok
        ));
        let mut fake = Broker::default();
        let now = Instant::now();
        let mut router = streaming_router();
        router.set_launch_participants(bindings.clone());
        let target = "/launch?appid=7&mode=1280x720x60&rikey=00112233445566778899aabbccddeeff&rikeyid=17&alpha=1&beta=2";
        assert!(
            router
                .dispatch_with(
                    1,
                    request(target),
                    trusted_tls(b"certificate"),
                    now,
                    &mut |cmd| fake.send(cmd)
                )
                .is_none()
        );
        let mut calls = Vec::new();
        let mut consumer_open = None;
        let mut reply = None;
        for _ in 0..200 {
            let replies = router.poll_with_identity(
                now,
                &mut |command| match command {
                    command @ Command::CallApi { .. } => match real
                        .dispatch("amaterasu.nvhttp", command)
                    {
                        Response::Accepted { id } => Ok(Response::Accepted { id: id + 1_000_000 }),
                        response => Ok(response),
                    },
                    command @ Command::Request { .. }
                        if matches!(&command, Command::Request { service, .. } if service == "amaterasu.rtsp/1") =>
                    {
                        match real.dispatch("amaterasu.nvhttp", command) {
                            Response::Accepted { id } => {
                                Ok(Response::Accepted { id: id + 1_000_000 })
                            }
                            response => Ok(response),
                        }
                    }
                    Command::TakeResult { request } if request >= 1_000_000 => Ok(real.dispatch(
                        "amaterasu.nvhttp",
                        Command::TakeResult {
                            request: request - 1_000_000,
                        },
                    )),
                    Command::Cancel { request } if request >= 1_000_000 => Ok(real.dispatch(
                        "amaterasu.nvhttp",
                        Command::Cancel {
                            request: request - 1_000_000,
                        },
                    )),
                    command => fake.send(command),
                },
                |_, _| true,
            );
            if let Some((_, received)) = replies.into_iter().next() {
                reply = Some(received);
                break;
            }
            if let Response::Request {
                id,
                service,
                payload,
                ..
            } = real.dispatch(provider, Command::Poll {})
            {
                if service == "amaterasu.rtsp/1" {
                    if payload["action"] == "launch_option_capabilities" {
                        assert!(matches!(
                            real.dispatch(
                                provider,
                                Command::Complete {
                                    request: id,
                                    payload: fixture_option_capabilities()
                                }
                            ),
                            Response::Ok
                        ));
                        continue;
                    }
                    assert_eq!(payload["action"], "open");
                    assert_eq!(
                        payload["launch_options"][0]["namespace"],
                        "org.example.alpha"
                    );
                    assert_eq!(
                        payload["launch_options"][1]["namespace"],
                        "org.example.beta"
                    );
                    let response = json!({"ok":true,"ticket":1,"encoder":payload["capture"]["encoder"],"sw_preset":payload["sw_preset"],"sw_tune":payload["sw_tune"],"min_threads":payload["min_threads"],"qp":payload["qp"],"nvenc_settings":payload["nvenc_settings"],"nvenc_spatial_aq":payload["nvenc_spatial_aq"],"rtsp_url":"rtspenc://127.0.0.1:48010","state":"ready","frame_limiter_settings":payload["frame_limiter_settings"],"frame_limiter_executable_path":payload["frame_limiter_executable_path"],"display_policy_snapshot":payload["display_policy_snapshot"],"display_request":payload["display_request"],"failure":null,"cleanup_error":null});
                    consumer_open = Some(payload);
                    assert!(matches!(
                        real.dispatch(
                            provider,
                            Command::Complete {
                                request: id,
                                payload: response
                            }
                        ),
                        Response::Ok
                    ));
                    continue;
                }
                assert_eq!(payload["method"], "project");
                assert_eq!(payload["interface_major"], 1);
                assert_eq!(payload["interface_minor"], 0);
                let arguments = &payload["arguments"];
                assert_eq!(arguments["version"], 1);
                assert_eq!(arguments["resume"], false);
                let field = if service == bindings[0].service {
                    "alpha"
                } else {
                    "beta"
                };
                assert_eq!(arguments["fields"].as_object().unwrap().len(), 1);
                assert!(arguments["fields"].get(field).is_some());
                assert!(
                    arguments
                        .to_string()
                        .find("00112233445566778899aabbccddeeff")
                        .is_none()
                );
                calls.push(field.to_owned());
                let namespace = if field == "alpha" {
                    "org.example.alpha"
                } else {
                    "org.example.beta"
                };
                assert!(matches!(real.dispatch(provider, Command::Complete { request:id, payload:json!({"namespace":namespace,"version":1,"value":{"enabled":true}}) }), Response::Ok));
            }
        }
        assert_eq!(calls, ["alpha", "beta"]);
        assert_eq!(reply.expect("launch reply").status, 200);
        let open = consumer_open.expect("selected consumer stream open");
        assert_eq!(open["launch_options"][0]["namespace"], "org.example.alpha");
        assert_eq!(open["launch_options"][1]["namespace"], "org.example.beta");
    }
}

#[test]
fn launch_participant_revocation_and_disconnect_discard_late_results() {
    use std::cell::Cell;
    for disconnect in [false, true] {
        let now = Instant::now();
        let mut router = streaming_router();
        router.set_launch_participants(vec![neutral_launch_binding("alpha", "org.example.alpha")]);
        let mut broker = Broker::default();
        let admitted = Cell::new(0);
        let cancelled = Cell::new(0);
        let mut send = |command| match command {
            Command::CallApi { .. } => {
                admitted.set(admitted.get() + 1);
                Ok(Response::Accepted { id: 1_000_000 })
            }
            Command::Cancel { request: 1_000_000 } => {
                cancelled.set(cancelled.get() + 1);
                Ok(Response::Ok)
            }
            Command::ReleaseResult { request: 1_000_000 } => Ok(Response::Ok),
            Command::TakeResult { request: 1_000_000 } => Ok(Response::Result {
                payload: json!({"namespace":"org.example.alpha","version":1,"value":{"enabled":true}}),
            }),
            command => broker.send(command),
        };
        let target = "/launch?appid=7&mode=1280x720x60&rikey=00112233445566778899aabbccddeeff&rikeyid=17&alpha=1";
        assert!(
            router
                .dispatch_with(
                    9,
                    request(target),
                    trusted_tls(b"certificate"),
                    now,
                    &mut send
                )
                .is_none()
        );
        for _ in 0..10 {
            router.poll_with_identity(now, &mut send, |_, _| true);
            if admitted.get() > 0 {
                break;
            }
        }
        assert_eq!(admitted.get(), 1);
        if disconnect {
            router.cancel_with(9, now, &mut send);
        } else {
            router.poll_with_identity(now, &mut send, |_, permission| {
                !matches!(permission, Permission::LaunchWithEntitlements(_))
            });
        }
        assert_eq!(cancelled.get(), 1);
        assert!(
            router
                .poll_with_identity(now + Duration::from_secs(1), &mut send, |_, _| true)
                .is_empty()
        );
        assert!(!router.launches.busy());
    }
}

#[test]
fn unknown_launch_participant_admission_is_held_until_deadline_without_retry_after_disconnect() {
    use std::cell::Cell;
    let now = Instant::now();
    let mut router = streaming_router();
    router.set_launch_participants(vec![neutral_launch_binding("alpha", "org.example.alpha")]);
    let mut broker = Broker::default();
    let attempts = Cell::new(0);
    let mut send = |command| match command {
        Command::CallApi { .. } => {
            attempts.set(attempts.get() + 1);
            Err("lost admission".into())
        }
        command => broker.send(command),
    };
    let target = "/launch?appid=7&mode=1280x720x60&rikey=00112233445566778899aabbccddeeff&rikeyid=17&alpha=1";
    assert!(
        router
            .dispatch_with(
                9,
                request(target),
                trusted_tls(b"certificate"),
                now,
                &mut send
            )
            .is_none()
    );
    for _ in 0..10 {
        router.poll_with_identity(now, &mut send, |_, _| true);
        if attempts.get() > 0 {
            break;
        }
    }
    assert_eq!(attempts.get(), 1);
    router.cancel_with(9, now, &mut send);
    assert_eq!(router.jobs.len(), 1);
    router.poll_with_identity(now + Duration::from_secs(1), &mut send, |_, _| true);
    assert_eq!(attempts.get(), 1);
    assert_eq!(router.jobs.len(), 1);
    router.poll_with_identity(now + Duration::from_secs(6), &mut send, |_, _| true);
    assert_eq!(attempts.get(), 1);
    assert!(router.jobs.is_empty());
    assert!(!router.launches.busy());
}

#[test]
fn resume_invokes_approved_participant_and_projects_new_generation() {
    let now = Instant::now();
    let mut router = streaming_router_with_desktop(true);
    router.set_launch_participants(vec![neutral_launch_binding("alpha", "org.example.alpha")]);
    let mut broker = Broker::default();
    let initial = "/launch?appid=2147483647&mode=1280x720x60&rikey=00112233445566778899aabbccddeeff&rikeyid=17";
    assert!(
        router
            .dispatch_with(
                1,
                request(initial),
                trusted_tls(b"certificate"),
                now,
                &mut |cmd| broker.send(cmd)
            )
            .is_none()
    );
    let (_, initial_reply) = poll_until_reply(&mut router, &mut broker, now);
    assert_eq!(initial_reply.status, 200);
    router.reply_delivered(1);
    let resume = "/resume?appid=2147483647&mode=1280x720x60&rikey=11112233445566778899aabbccddeeff&rikeyid=18&alpha=1";
    assert!(
        router
            .dispatch_with(
                2,
                request(resume),
                trusted_tls(b"certificate"),
                now,
                &mut |cmd| broker.send(cmd)
            )
            .is_none()
    );
    let mut seen = false;
    let mut reply = None;
    for _ in 0..200 {
        let replies = router.poll_with_identity(now, &mut |command| match command {
            Command::CallApi { payload, .. } => {
                assert_eq!(payload["resume"], true);
                assert_eq!(payload["fields"], json!({"alpha":"1"}));
                seen = true;
                Ok(Response::Accepted { id:1_000_000 })
            }
            Command::TakeResult { request:1_000_000 } => Ok(Response::Result { payload:json!({"namespace":"org.example.alpha","version":1,"value":{"enabled":true}}) }),
            command => broker.send(command),
        }, |_, _| true);
        if let Some((_, response)) = replies.into_iter().next() {
            reply = Some(response);
            break;
        }
    }
    assert!(seen);
    assert_eq!(reply.expect("resume reply").status, 200);
    let open = broker
        .accepted
        .iter()
        .rev()
        .find(|payload| payload["action"] == "open")
        .expect("resumed stream open");
    assert_eq!(open["launch_options"][0]["namespace"], "org.example.alpha");
}

#[test]
fn invalid_launch_participant_reply_never_starts_a_session() {
    for invalid in [
        json!({"namespace":"org.example.other","version":1,"value":{"enabled":true}}),
        json!({"namespace":"org.example.alpha","version":2,"value":{"enabled":true}}),
        json!({"namespace":"org.example.alpha","version":1,"value":{"unknown":true}}),
    ] {
        let now = Instant::now();
        let mut router = streaming_router();
        router.set_launch_participants(vec![neutral_launch_binding("alpha", "org.example.alpha")]);
        let mut broker = Broker::default();
        let target = "/launch?appid=7&mode=1280x720x60&rikey=00112233445566778899aabbccddeeff&rikeyid=17&alpha=1";
        assert!(
            router
                .dispatch_with(
                    9,
                    request(target),
                    trusted_tls(b"certificate"),
                    now,
                    &mut |cmd| broker.send(cmd)
                )
                .is_none()
        );
        let mut reply = None;
        for _ in 0..15 {
            let replies = router.poll_with_identity(
                now,
                &mut |command| match command {
                    Command::CallApi { .. } => Ok(Response::Accepted { id: 1_000_000 }),
                    Command::TakeResult { request: 1_000_000 } => Ok(Response::Result {
                        payload: invalid.clone(),
                    }),
                    command => broker.send(command),
                },
                |_, _| true,
            );
            if let Some((_, response)) = replies.into_iter().next() {
                reply = Some(response);
                break;
            }
        }
        assert_eq!(reply.expect("invalid option response").status, 502);
        assert!(!router.launches.busy());
        assert!(
            !broker
                .accepted
                .iter()
                .any(|payload| payload["action"] == "open")
        );
    }
}

#[test]
#[ignore = "requires independently built neutral fixture and runtime libraries"]
fn independently_loaded_sdk_fixture_serves_unchanged_launch_consumer() {
    use amaterasu_contracts::{
        ApiFieldKind, ApiMethod, ApiMethodApproval, ApiPrivilege, ApiSchema, Grants, RuntimeConfig,
    };
    use amaterasu_module_sdk::abi::{
        BorrowedBytes, HostApi, ModuleApi, ModuleEntry, RuntimeApi, RuntimeEntry, STATUS_OK,
    };
    use std::{ffi::c_void, mem::MaybeUninit, path::PathBuf};
    let runtime_path =
        PathBuf::from(std::env::var_os("AMATERASU_TEST_RUNTIME").expect("runtime path"));
    let fixture_path =
        PathBuf::from(std::env::var_os("AMATERASU_TEST_LAUNCH_FIXTURE").expect("fixture path"));
    // SAFETY: these are explicitly supplied test artifacts and remain loaded until all instances are destroyed.
    let runtime_library = unsafe { libloading::Library::new(runtime_path) }.unwrap();
    let fixture_library = unsafe { libloading::Library::new(fixture_path) }.unwrap();
    // SAFETY: both libraries export their documented, versioned entry points.
    let runtime_entry: libloading::Symbol<RuntimeEntry> =
        unsafe { runtime_library.get(b"amaterasu_runtime_v1\0") }.unwrap();
    let fixture_entry: libloading::Symbol<ModuleEntry> =
        unsafe { fixture_library.get(b"amaterasu_module_v1\0") }.unwrap();
    // SAFETY: the entry points return static ABI tables owned by their loaded libraries.
    let runtime_api = unsafe { &*runtime_entry() };
    let fixture_api = unsafe { &*fixture_entry() };
    assert!(runtime_api.header.supports::<RuntimeApi>());
    assert!(fixture_api.header.supports::<ModuleApi>());
    let shape = |fields: &[(&str, ApiFieldKind)]| ApiSchema {
        required: fields
            .iter()
            .map(|(name, kind)| ((*name).into(), kind.clone()))
            .collect(),
        optional: BTreeMap::new(),
        allow_extra: false,
    };
    let bindings = vec![
        neutral_launch_binding("alpha", "org.example.alpha"),
        neutral_launch_binding("beta", "org.example.beta"),
    ];
    assert!(launch_participants::validate(&bindings).is_ok());
    for provider in ["org.example.fixture-one", "org.example.fixture-two"] {
        let mut config = RuntimeConfig::default();
        let mut services: BTreeSet<String> = bindings
            .iter()
            .map(|binding| binding.service.clone())
            .collect();
        services.insert("amaterasu.rtsp/1".into());
        config.principals.insert(
            "amaterasu.nvhttp".into(),
            Grants {
                call: services.clone(),
                ..Default::default()
            },
        );
        config.principals.insert(
            provider.into(),
            Grants {
                provide: services,
                ..Default::default()
            },
        );
        for binding in &bindings {
            config.api_methods.push(ApiMethodApproval {
                provider: provider.into(),
                callers: ["amaterasu.nvhttp".into()].into(),
                method: ApiMethod {
                    service: binding.service.clone(),
                    method: binding.method.clone(),
                    interface_major: 1,
                    interface_minor: 0,
                    request: shape(&[
                        ("version", ApiFieldKind::Integer),
                        ("resume", ApiFieldKind::Boolean),
                        ("fields", ApiFieldKind::Object),
                    ]),
                    response: shape(&[
                        ("namespace", ApiFieldKind::String),
                        ("version", ApiFieldKind::Integer),
                        ("value", ApiFieldKind::Object),
                    ]),
                    privilege: ApiPrivilege::User,
                },
            });
        }
        let config_bytes = serde_json::to_vec(&config).unwrap();
        let mut runtime: *mut c_void = std::ptr::null_mut();
        // SAFETY: input and output remain valid during creation and runtime is destroyed after the fixture.
        assert_eq!(
            unsafe { (runtime_api.create)(BorrowedBytes::new(&config_bytes), &mut runtime) },
            STATUS_OK
        );
        let endpoint = |principal: &str| {
            let mut output = MaybeUninit::<HostApi>::uninit();
            // SAFETY: runtime is live, principal bytes and output are valid during the call.
            assert_eq!(
                unsafe {
                    (runtime_api.endpoint)(
                        runtime,
                        BorrowedBytes::new(principal.as_bytes()),
                        output.as_mut_ptr(),
                    )
                },
                STATUS_OK
            );
            // SAFETY: a successful endpoint call initialized the table.
            unsafe { output.assume_init() }
        };
        let owner_endpoint = endpoint("amaterasu.nvhttp");
        let provider_endpoint = endpoint(provider);
        let mut fixture: *mut c_void = std::ptr::null_mut();
        // SAFETY: endpoint and output are valid, and fixture is retired before runtime destruction.
        assert_eq!(
            unsafe { (fixture_api.create)(&provider_endpoint, &mut fixture) },
            STATUS_OK
        );
        assert!(!fixture.is_null());
        // SAFETY: the runtime and endpoint outlive this client and all calls.
        let owner = unsafe { RuntimeClient::from_endpoint(owner_endpoint) };
        let mut fake = Broker::default();
        let now = Instant::now();
        let mut router = streaming_router();
        router.set_launch_participants(bindings.clone());
        let target = "/launch?appid=7&mode=1280x720x60&rikey=00112233445566778899aabbccddeeff&rikeyid=17&alpha=1&beta=2";
        assert!(
            router
                .dispatch_with(
                    1,
                    request(target),
                    trusted_tls(b"certificate"),
                    now,
                    &mut |cmd| fake.send(cmd)
                )
                .is_none()
        );
        let mut response = None;
        for _ in 0..200 {
            let replies = router.poll_with_identity(now, &mut |command| match command {
                command @ Command::CallApi { .. } => match owner.send_ephemeral(command)? {
                    Response::Accepted { id } => Ok(Response::Accepted { id:id + 1_000_000 }),
                    other => Ok(other),
                },
                command @ Command::Request { .. } if matches!(&command, Command::Request { service, .. } if service == "amaterasu.rtsp/1") => match owner.send_ephemeral(command)? {
                    Response::Accepted { id } => Ok(Response::Accepted { id:id + 1_000_000 }),
                    other => Ok(other),
                },
                Command::TakeResult { request } if request >= 1_000_000 => owner.send_ephemeral(Command::TakeResult { request:request - 1_000_000 }),
                Command::Cancel { request } if request >= 1_000_000 => owner.send_ephemeral(Command::Cancel { request:request - 1_000_000 }),
                command => fake.send(command),
            }, |_, _| true);
            // SAFETY: fixture remains live and lifecycle calls are serialized by this test.
            assert_eq!(unsafe { (fixture_api.tick)(fixture) }, STATUS_OK);
            if let Some((_, reply)) = replies.into_iter().next() {
                response = Some(reply);
                break;
            }
        }
        assert_eq!(response.expect("loaded fixture launch reply").status, 200);
        // SAFETY: fixture is destroyed once, before the runtime and loaded libraries.
        unsafe { (fixture_api.destroy)(fixture) };
        // SAFETY: all endpoint users are retired before runtime destruction.
        unsafe { (runtime_api.destroy)(runtime) };
    }
}

#[test]
fn oversized_participant_output_stops_before_application_preparation() {
    let now = Instant::now();
    let mut router = streaming_router();
    router.set_launch_participants(vec![neutral_launch_binding("alpha", "org.example.alpha")]);
    let mut broker = Broker::default();
    let target = "/launch?appid=7&mode=1280x720x60&rikey=00112233445566778899aabbccddeeff&rikeyid=17&alpha=1";
    assert!(
        router
            .dispatch_with(
                9,
                request(target),
                trusted_tls(b"certificate"),
                now,
                &mut |cmd| broker.send(cmd)
            )
            .is_none()
    );
    let mut reply = None;
    for _ in 0..15 {
        let replies = router.poll_with_identity(now, &mut |command| match command {
            Command::CallApi { .. } => Ok(Response::Accepted { id:1_000_000 }),
            Command::TakeResult { request:1_000_000 } => Ok(Response::Result { payload:json!({"namespace":"org.example.alpha","version":1,"value":{"enabled":true,"oversized":"x".repeat(4096)}}) }),
            command => broker.send(command),
        }, |_, _| true);
        if let Some((_, response)) = replies.into_iter().next() {
            reply = Some(response);
            break;
        }
    }
    assert_eq!(reply.expect("oversized participant response").status, 502);
    assert!(!router.launches.busy());
    assert!(!broker.accepted.iter().any(|payload| matches!(
        payload["action"].as_str(),
        Some("preflight_export" | "launch_export" | "open")
    )));
}

#[test]
fn unsupported_or_invalid_stream_declaration_rejects_before_participants_and_preparation() {
    let cases = [
        json!({"version":1,"options":[]}),
        json!({"version":1,"options":[{"namespace":"org.example.alpha","version":1,"schema":{"id":"org.example.option","major":1,"minor":0,"digest":"0".repeat(64)}}]}),
        json!({"version":2,"options":[]}),
        json!({"version":1,"options":[],"extra":"x".repeat(8192)}),
    ];
    for declaration in cases {
        let now = Instant::now();
        let mut router = streaming_router();
        router.set_launch_participants(vec![neutral_launch_binding("alpha", "org.example.alpha")]);
        let mut broker = Broker::default();
        let target = "/launch?appid=7&mode=1280x720x60&rikey=00112233445566778899aabbccddeeff&rikeyid=17&alpha=1";
        assert!(
            router
                .dispatch_with(
                    1,
                    request(target),
                    trusted_tls(b"certificate"),
                    now,
                    &mut |cmd| broker.send(cmd)
                )
                .is_none()
        );
        let mut called = false;
        let mut response = None;
        for _ in 0..20 {
            let replies = router.poll_with_identity(
                now,
                &mut |command| match command {
                    Command::CallApi { .. } => {
                        called = true;
                        Ok(Response::error("unexpected", "participant called"))
                    }
                    Command::TakeResult { request }
                        if broker
                            .requests
                            .get(&request)
                            .is_some_and(|v| v["action"] == "launch_option_capabilities") =>
                    {
                        Ok(Response::Result {
                            payload: declaration.clone(),
                        })
                    }
                    command => broker.send(command),
                },
                |_, _| true,
            );
            if let Some((_, reply)) = replies.into_iter().next() {
                response = Some(reply);
                break;
            }
        }
        assert_eq!(response.expect("rejection").status, 502);
        assert!(!called);
        assert!(!router.launches.busy());
        assert!(!broker.accepted.iter().any(|p| matches!(
            p["action"].as_str(),
            Some("preflight_export" | "launch_export" | "open")
        )));
    }
}

#[test]
fn pending_stream_declaration_is_retired_on_disconnect_or_deadline() {
    for disconnect in [true, false] {
        let now = Instant::now();
        let mut router = streaming_router();
        router.set_launch_participants(vec![neutral_launch_binding("alpha", "org.example.alpha")]);
        let mut broker = Broker::default();
        let target = "/launch?appid=7&mode=1280x720x60&rikey=00112233445566778899aabbccddeeff&rikeyid=17&alpha=1";
        assert!(
            router
                .dispatch_with(
                    1,
                    request(target),
                    trusted_tls(b"certificate"),
                    now,
                    &mut |cmd| broker.send(cmd)
                )
                .is_none()
        );
        for _ in 0..10 {
            router.poll_with_identity(
                now,
                &mut |command| match command {
                    Command::TakeResult { request }
                        if broker
                            .requests
                            .get(&request)
                            .is_some_and(|v| v["action"] == "launch_option_capabilities") =>
                    {
                        Ok(Response::Pending)
                    }
                    command => broker.send(command),
                },
                |_, _| true,
            );
            if broker
                .requests
                .values()
                .any(|v| v["action"] == "launch_option_capabilities")
            {
                break;
            }
        }
        let request_id = *broker
            .requests
            .iter()
            .find(|(_, v)| v["action"] == "launch_option_capabilities")
            .unwrap()
            .0;
        if disconnect {
            router.cancel_with(1, now, &mut |command| broker.send(command));
        } else {
            let replies = router.poll_with_identity(
                now + Duration::from_secs(6),
                &mut |command| broker.send(command),
                |_, _| true,
            );
            assert_eq!(replies[0].1.status, 504);
        }
        assert!(broker.cancelled.contains(&request_id));
        assert!(!router.launches.busy());
        assert!(!broker.accepted.iter().any(|p| matches!(
            p["action"].as_str(),
            Some("preflight_export" | "launch_export" | "open")
        )));
    }
}
