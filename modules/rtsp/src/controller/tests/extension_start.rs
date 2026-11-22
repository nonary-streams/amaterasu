use super::*;
use amaterasu_contracts::media_profile::{ChromaSampling, VideoProfile, VideoProfileSnapshot};

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
    extension_record_with_chroma(hdr, ChromaSampling::Yuv420)
}

fn extension_record_with_chroma(hdr: bool, chroma: ChromaSampling) -> Record {
    let profile = launch_profile(hdr);
    let mut full_chroma = profile.clone();
    full_chroma.media_format.as_mut().unwrap().chroma_sampling = ChromaSampling::Yuv444;
    let snapshot = VideoProfileSnapshot {
        profiles: vec![profile, full_chroma],
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
    let announcement = if chroma == ChromaSampling::Yuv444 {
        announcement.replace(
            "x-ss-video[0].chromaSamplingType:0",
            "x-ss-video[0].chromaSamplingType:1",
        )
    } else {
        announcement
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
    record.extension_offer = snapshot.profiles;
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
        expected["extension_profile"] = json!(record.extension_offer.first().unwrap());
        expected["color_mode"] = json!("bt601_limited");
        assert_eq!(payload["configuration"]["configuration"], expected);
        assert_eq!(
            payload["configuration"]["extension_wire_bitrate_kbps"],
            695_000
        );
        assert_eq!(
            payload["configuration"]["configuration"]["video_stream_format"]["dynamic_range"],
            if hdr { "hdr" } else { "sdr" }
        );
    }
}

#[test]
fn moonlight_pyrowave_444_announce_requires_an_exact_offer_and_preserves_chroma_at_play() {
    for hdr in [false, true] {
        let mut record = extension_record_with_chroma(hdr, ChromaSampling::Yuv444);
        let announce = record.extension_announce.clone().unwrap();
        let payload = record.authenticated_start_payload(23).unwrap();
        let configuration = &payload["configuration"]["configuration"];
        assert_eq!(
            configuration["video_stream_format"]["chroma_sampling"],
            "yuv444"
        );
        assert_eq!(
            configuration["extension_profile"],
            json!(record.extension_offer[1])
        );
        record.extension_offer.pop();
        assert!(!record.accept_extension_announce(&announce));
    }
}

#[test]
fn pyrowave_wire_budget_honors_explicit_host_limit_and_legacy_requests() {
    let mut record = extension_record(true);
    let mut announce = record.extension_announce.clone().unwrap();
    record.config.max_bitrate_kbps = Some(80_000);
    assert!(record.accept_extension_announce(&announce));
    let start = record.authenticated_start_payload(23).unwrap();
    assert_eq!(
        start["configuration"]["extension_wire_bitrate_kbps"],
        80_000
    );

    record.extension_wire_budget = Some(false);
    assert!(!record.accept_extension_announce(&announce));
    let mut bounded_legacy = announce.clone();
    bounded_legacy.video.bitrate_kbps = 19_600;
    assert!(record.accept_extension_announce(&bounded_legacy));
    let start = record.authenticated_start_payload(23).unwrap();
    assert!(
        start["configuration"]
            .get("extension_wire_bitrate_kbps")
            .is_none()
    );
    assert_eq!(
        start["configuration"]["configuration"]["bitrate"],
        19_600_000
    );

    record.extension_wire_budget = Some(true);
    announce.video.configured_bitrate_kbps = None;
    assert!(!record.accept_extension_announce(&announce));
    announce.video.bitrate_kbps = 19_600;
    assert!(record.accept_extension_announce(&announce));
    let start = record.authenticated_start_payload(23).unwrap();
    assert!(
        start["configuration"]
            .get("extension_wire_bitrate_kbps")
            .is_none()
    );
    assert_eq!(
        start["configuration"]["configuration"]["bitrate"],
        19_600_000
    );
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
            json!(controller.records[&5].extension_offer.first().unwrap())
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
