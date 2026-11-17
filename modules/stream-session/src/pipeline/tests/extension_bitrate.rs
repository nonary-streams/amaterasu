use super::*;

#[test]
fn extension_start_lowers_the_probed_bitrate_without_changing_other_parameters() {
    use amaterasu_media::*;
    let profile = json!({"codec":{"namespace_id":7,"value":8,"schema":1},
        "framing":{"namespace_id":7,"value":9,"schema":1},"wire_selector":4,
        "payload_type":100,"encoding_name":"EXAMPLE"});
    let route: crate::model::ExtensionRoute = serde_json::from_value(json!({"profile":profile,
        "source_id":42,"submitted":true,"encoder_provider":"fixture",
        "encoder":{"clsid":"source","friendly_name":"Source","hardware_url":null},
        "video_unit_service":"example.units/1","framer_service":"example.framer/1",
        "plan_transport_service":"example.plan/1"}))
    .unwrap();
    let limits = VideoFramingLimits {
        size: size_of::<VideoFramingLimits>() as u32,
        version: VIDEO_FRAMING_LIMITS_VERSION,
        packet_size: 1024,
        crypto_overhead: 16,
        network_overhead: 28,
        allowed_extensions: 0,
        max_datagrams: 64,
        max_queue_bytes: 65536,
        min_parity_shards: 2,
        max_fec_percentage: 255,
        max_wire_bytes: 65536,
        min_fec_percentage: 20,
        max_fec_blocks: 1,
        fec_policy_flags: 0,
        reserved: 0,
    };
    for (field, value, accepted) in [
        ("bitrate", json!(19_600_000), true),
        ("bitrate", json!(100_000), true),
        ("bitrate", json!(100_000_000), true),
        ("bitrate", json!(0), false),
        ("bitrate", json!(99_999), false),
        ("bitrate", json!(100_000_001), false),
        ("width", json!(1282), false),
        ("fps", json!(59), false),
        ("max_reference_frames", json!(1), false),
    ] {
        let mut host = Harness::new();
        host.pipeline.settings.allowed_principals = vec!["amaterasu.rtsp".into()];
        host.pipeline.settings.extension_route = Some(route.clone());
        host.broker.extension_probe_supported = true;
        host.extension_probe_limits = Some(limits);
        let mut probe = start(0)["configuration"]["configuration"].clone();
        probe["bitrate"] = json!(100_000_000);
        let mut request = prepare();
        request["stream_audio"] = json!(false);
        request["extension_probe"] = probe.clone();
        let prepared = host.response_from(1, "amaterasu.rtsp", request);
        assert_eq!(prepared["extension_profile"], profile);
        let session = prepared["session"].as_u64().unwrap();
        let mut request = start(session);
        request["configuration"]["client_feature_flags"] = json!(0);
        request["configuration"]["configuration"] = probe;
        request["configuration"]["configuration"]["extension_profile"] = profile.clone();
        request["configuration"]["configuration"][field] = value.clone();
        let result = host.response_from(2, "amaterasu.rtsp", request);
        let video =
            host.broker.calls.iter().find(|call| {
                call.service == "amaterasu.video/1" && call.payload["action"] == "open"
            });
        if accepted {
            assert_eq!(result["state"], "active", "{field}={value}: {result}");
            assert_eq!(video.unwrap().payload["configuration"]["bitrate"], value);
            assert_eq!(host.plane.borrow().planned_planes, 1);
        } else {
            assert_ne!(result["ok"], true, "{field}={value}: {result}");
            assert!(video.is_none(), "rejected configuration reached video open");
            assert_eq!(host.plane.borrow().planned_planes, 0);
        }
    }
}
