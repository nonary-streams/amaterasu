use super::*;
use amaterasu_contracts::media_profile::{VideoProfile, VideoProfileSnapshot};

/// The PyroWave profile a launch is offered: 10-bit HDR, or the 8-bit SDR
/// profile Moonlight requests when HDR is off.
fn launch_profile(hdr: bool) -> VideoProfile {
    let mut profile: VideoProfile =
        serde_json::from_str(include_str!("../../../../../scripts/pyrowave/profile.json")).unwrap();
    if !hdr {
        profile.codec.value = 1;
        profile.media_format = serde_json::from_value(
            json!({"bit_depth":8,"dynamic_range":"sdr","chroma_sampling":"yuv420"}),
        )
        .unwrap();
    }
    profile
}

fn extension_record(hdr: bool) -> Record {
    let profile = launch_profile(hdr);
    let snapshot = VideoProfileSnapshot {
        profiles: vec![profile.clone()],
    };
    let fixture =
        include_str!("../../../../../crates/gamestream/src/sdp/fixtures/moonlight-pyrowave10.sdp");
    let announcement = if hdr {
        fixture.to_owned()
    } else {
        fixture.replace(
            "x-nv-video[0].dynamicRangeMode:1",
            "x-nv-video[0].dynamicRangeMode:0",
        )
    };
    let announce =
        amaterasu_gamestream::sdp::parse_profile_announce(announcement.as_bytes(), &snapshot)
            .unwrap();
    let (mut record, _) = announced_profile_record();
    record.config.profile.width = 2560;
    record.config.profile.height = 1440;
    record.config.profile.fps = 120;
    record.config.profile.bitrate_kbps = 100000;
    record.config.profile.packet_size = 1360;
    record.config.profile.hdr = hdr;
    record.config.sw_preset = Some("superfast".into());
    record.config.sw_tune = Some("zerolatency".into());
    record.extension_offer = Some(profile);
    assert!(record.accept_extension_announce(&announce));
    record
}

fn select_lifecycle(record: &mut Record) {
    record.config.lifecycle_session_target = serde_json::from_value(
        json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1}),
    )
    .ok();
    record.config.lifecycle_settings_revision = Some("7".into());
    record.config.lifecycle_intent = Some("initial".into());
    record.lifecycle_input_required = Some(false);
}

fn prepared_capture(hdr: bool) -> Value {
    json!({"status":"succeeded","operation_id":"rtsp-0000000000000005-lifecycle-prepare","transition_id":"transition",
        "target":{"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1},
        "candidate":{"capture_provider":"amaterasu.capture-pipewire/1","selector_schema":"amaterasu.kwin-wl-output/1","selector":{"output":"kwin:Virtual-12"},
            "resource":{"provider":"test.provider","resource_id":"output","incarnation":"one","revision":1,"execution_domain":"desktop","mutation_domain":null,"mutation_domains":["desktop"]},
            "mode":{"width":2560,"height":1440,"refresh_millihz":120000,"hdr":hdr},"plan_revision":1,"geometry_revision":1,"generation":1,"fallback":"forbidden"}})
}

#[test]
fn moonlight_generated_extension_request_adds_color_to_the_probed_configuration() {
    for hdr in [false, true] {
        let record = extension_record(hdr);
        let payload = record.authenticated_start_payload(23).unwrap();
        let mut expected = record
            .config
            .extension_configuration(record.config.launch_extension_format(10));
        expected["extension_profile"] = json!(record.extension_offer.as_ref().unwrap());
        expected["color_mode"] = json!("bt601_limited");
        assert_eq!(payload["configuration"]["configuration"], expected);
        assert_eq!(
            payload["configuration"]["configuration"]["video_stream_format"]["dynamic_range"],
            if hdr { "hdr" } else { "sdr" }
        );
    }
}

#[test]
fn extension_play_prepares_lifecycle_without_substituting_a_legacy_codec() {
    for hdr in [false, true] {
        let mut record = extension_record(hdr);
        select_lifecycle(&mut record);
        assert!(record.negotiated_video.is_none());
        assert!(record.negotiated_video_stream_format.is_none());
        let start = record.authenticated_start_payload(23).unwrap();
        let mut controller = Controller::new(Settings::default()).unwrap();
        controller.schedule_arm(&mut record, start, 8, 9, 0);
        assert!(
            controller
                .job
                .as_ref()
                .is_some_and(|job| job.phase == Phase::LifecyclePrepare),
            "extension PLAY must reach lifecycle preparation"
        );
        assert!(record.lifecycle_prepare_outstanding);
        controller.records.insert(record.id, record);
        let mut endpoint = Endpoint {
            verifier_reply: Some(prepared_capture(hdr)),
            ..Endpoint::default()
        };
        let runtime = client(&mut endpoint);
        for now in 1..=3 {
            controller.advance_job(&runtime, now);
        }
        assert!(endpoint.commands.iter().any(|command| matches!(command,
            Command::Request { service, payload, .. }
                if service == "amaterasu.session-lifecycle/1"
                && payload["requested_video"] == json!({"width":2560,"height":1440,"refresh_millihz":120000,"hdr":hdr})
        )));
        let started = endpoint
            .commands
            .iter()
            .find_map(|command| match command {
                Command::Request {
                    service, payload, ..
                } if service == "amaterasu.stream-session/1"
                    && payload["action"] == "start_when_authenticated" =>
                {
                    Some(payload)
                }
                _ => None,
            })
            .expect("validated lifecycle preparation must reach video startup");
        assert_eq!(
            started["configuration"]["configuration"]["extension_profile"],
            json!(controller.records[&5].extension_offer.as_ref().unwrap())
        );
        assert_eq!(
            started["configuration"]["configuration"]["video_stream_format"]["dynamic_range"],
            if hdr { "hdr" } else { "sdr" }
        );
        assert_eq!(
            started["configuration"]["lifecycle_candidate"],
            prepared_capture(hdr)["candidate"]
        );
        assert!(started["prepare_video_before_play"].as_bool().unwrap());
    }
}

#[test]
fn extension_lifecycle_capture_must_match_exact_geometry_range_and_generation() {
    for hdr in [false, true] {
        for (field, value) in [
            ("width", json!(2562)),
            ("height", json!(1442)),
            ("refresh_millihz", json!(119999)),
            ("hdr", json!(!hdr)),
            ("generation", json!(2)),
        ] {
            let mut record = extension_record(hdr);
            select_lifecycle(&mut record);
            record.lifecycle_prepare_outstanding = true;
            let mut payload = prepared_capture(hdr);
            if field == "generation" {
                payload["candidate"][field] = value;
            } else {
                payload["candidate"]["mode"][field] = value;
            }
            assert!(matches!(
                preparation::apply_response(&mut record, &payload),
                preparation::PreparationResolution::Unconfirmed
            ));
            assert!(record.lifecycle_prepare_outstanding);
            assert!(record.lifecycle_target.is_none());
        }
    }
}

#[test]
fn extension_accepts_the_client_bandwidth_rate_below_the_launch_ceiling() {
    for hdr in [false, true] {
        let mut record = extension_record(hdr);
        let mut announce = record.extension_announce.clone().unwrap();
        // The failed 4K60 launch announced 19,600 kbps against a 100,000 kbps
        // prepared ceiling. Geometry and the exact offered codec are unchanged.
        announce.video.bitrate_kbps = 19_600;
        assert!(record.accept_extension_announce(&announce));
        let payload = record.authenticated_start_payload(23).unwrap();
        assert_eq!(
            payload["configuration"]["configuration"]["bitrate"],
            19_600_000
        );
        for invalid in [0, 99, 100_001, u32::MAX] {
            announce.video.bitrate_kbps = invalid;
            assert!(!record.accept_extension_announce(&announce));
            assert!(record.authenticated_start_payload(23).is_none());
        }
    }
}

#[test]
fn four_k_pyrowave_bandwidth_announcement_returns_200_and_reaches_play() {
    for hdr in [false, true] {
        let mut record = extension_record(hdr);
        record.config.profile.width = 3840;
        record.config.profile.height = 2160;
        record.config.profile.fps = 60;
        let snapshot = VideoProfileSnapshot {
            profiles: vec![record.extension_offer.clone().unwrap()],
        };
        let mut negotiation = Negotiation::with_video_profiles(
            TrustedLaunch {
                epoch: record.id,
                operation: record.config.operation_id.clone(),
                rtsp_target: record.url.clone(),
                session_token: record.session_token.clone(),
                encrypted_rtsp: true,
                audio_port: 49000,
                video_port: 49002,
                control_port: 49004,
                control_connect_data: record.config.connect_data,
                udp_discovery: None,
            },
            record.capabilities(),
            snapshot.clone(),
            snapshot,
        )
        .and_then(|negotiation| negotiation.with_failure_owner(&record.failure_scope))
        .unwrap();
        negotiation
            .handle(record.id, &request(Method::Options, &record.url, 1, false))
            .unwrap();
        let mut describe = request(Method::Describe, &record.url, 2, false);
        describe
            .headers
            .push(("accept".into(), "application/sdp".into()));
        negotiation.handle(record.id, &describe).unwrap();
        for (cseq, target) in [
            (3, "streamid=audio/0/0"),
            (4, "streamid=video/0/0"),
            (5, "streamid=control/13/0"),
        ] {
            let mut setup = request(Method::Setup, target, cseq, cseq > 3);
            setup.headers.push((
                "transport".into(),
                "unicast;X-GS-ClientPort=50000-50001".into(),
            ));
            negotiation.handle(record.id, &setup).unwrap();
        }
        let mut announcement = request(Method::Announce, "streamid=control/13/0", 6, true);
        announcement
            .headers
            .push(("content-type".into(), "application/sdp".into()));
        let fixture = include_str!(
            "../../../../../crates/gamestream/src/sdp/fixtures/moonlight-pyrowave10.sdp"
        );
        let fixture = fixture
            .replace("2560", "3840")
            .replace("1440", "2160")
            .replace("maxFPS:120", "maxFPS:60")
            .replace("100000", "19600");
        announcement.body = if hdr {
            fixture
        } else {
            fixture.replace("dynamicRangeMode:1", "dynamicRangeMode:0")
        }
        .into_bytes();
        let NegotiationStep::Action {
            ticket,
            action: Action::ExtensionAnnounce(announce),
        } = negotiation.handle(record.id, &announcement).unwrap()
        else {
            panic!("expected extension ANNOUNCE");
        };
        let accepted = record.accept_extension_announce(&announce);
        assert!(accepted, "lower extension rate must be accepted");
        let reply = negotiation.complete(record.id, ticket).unwrap();
        assert!(
            reply.starts_with(b"RTSP/1.0 200 "),
            "{}",
            String::from_utf8_lossy(&reply)
        );
        assert!(matches!(
            negotiation
                .handle(record.id, &request(Method::Play, "/", 7, true))
                .unwrap(),
            NegotiationStep::Action {
                action: Action::Play,
                ..
            }
        ));
        let start = record.authenticated_start_payload(23).unwrap();
        assert_eq!(start["configuration"]["configuration"]["width"], 3840);
        assert_eq!(
            start["configuration"]["configuration"]["bitrate"],
            19_600_000
        );
    }
}
