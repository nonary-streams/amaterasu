use super::*;

fn preparation(channels: u32) -> Value {
    let mut request = prepare();
    let (mask, layout) = match channels {
        6 => (0x3f, "5.1"),
        8 => (0x63f, "7.1"),
        _ => (3, "stereo"),
    };
    request["audio"] = json!({"channels":channels,"channel_mask":mask,"layout":layout,
        "packet_ms":5,"bitrate":if channels == 2 {128000} else {512000}});
    request
}

fn host_capture(session: u64, channels: u32) -> Value {
    let mut request = start(session);
    request["configuration"]["audio_endpoint"] = json!("host-playback-endpoint");
    request["configuration"]["capture_sink_without_routing"] = json!(true);
    request["configuration"]["host_audio_enabled"] = json!(true);
    request["configuration"]["channels"] = json!(channels);
    request
}

#[test]
fn regular_host_capture_negotiates_pcm_channels_without_selecting_a_virtual_sink_layout() {
    for channels in [2, 6, 8] {
        let mut host = Harness::new();
        let prepared = host.response(1, preparation(channels));
        let session = prepared["session"].as_u64().unwrap();
        let active = host.response(2, host_capture(session, channels));
        assert_eq!(active["state"], "active", "{active}");
        assert_eq!(
            host.pipeline.records[&session].config.audio.channels,
            channels
        );
        let audio = host
            .broker
            .calls
            .iter()
            .find(|call| {
                call.service == Resource::AudioCapture.service() && call.payload["action"] == "open"
            })
            .unwrap();
        assert_eq!(audio.payload["endpoint"], "host-playback-endpoint");
        assert_eq!(audio.payload["host_audio_enabled"], true);
        assert_eq!(audio.payload["capture_sink_without_routing"], true);
        assert!(audio.payload["virtual_sink"].is_null());
        assert!(audio.payload["channels"].is_null());
        assert_eq!(
            audio.payload["capture_format"],
            json!({"sample_rate":48000,"channels":channels,"sample_format":"f32"})
        );
        assert!(host.broker.calls.iter().all(|call| !matches!(
            call.service.as_str(),
            "amaterasu.display/1" | "amaterasu.virtual-display/1"
        )));
        assert_eq!(
            host.response(3, json!({"action":"stop","session":session}))["state"],
            "stopped"
        );
        assert_eq!(host.broker.leases(), 0);
        assert_eq!(host.broker.live_components(), 0);
    }
}

#[test]
fn host_surround_rejects_capture_format_or_sink_layout_substitution_and_retires_allocations() {
    for channels in [6, 8] {
        for invalid_sink_layout in [false, true] {
            let mut host = Harness::new();
            let session = host.response(1, preparation(channels))["session"]
                .as_u64()
                .unwrap();
            if invalid_sink_layout {
                host.broker.reported_audio_sink_channels = Some(channels);
            } else {
                host.broker.reported_audio_capture_channels = Some(2);
            }
            let rejected = host.response(2, host_capture(session, channels));
            assert_eq!(rejected["state"], "failed", "{rejected}");
            assert_eq!(
                rejected["error"],
                if invalid_sink_layout {
                    "audio_capture_policy_unverified"
                } else {
                    "unsupported_audio_format"
                }
            );
            assert_eq!(host.broker.leases(), 0);
            assert_eq!(host.broker.live_components(), 0);
            assert_eq!(host.plane.borrow().ticks, 0);
        }
    }
}

#[test]
fn lost_host_surround_open_response_is_reconciled_and_closed_without_sink_layout_mutation() {
    for channels in [6, 8] {
        let mut host = Harness::new();
        let session = host.response(1, preparation(channels))["session"]
            .as_u64()
            .unwrap();
        host.broker.fault = Some((
            format!("{}:open", Resource::AudioCapture.service()),
            Fault::AfterMutation,
        ));
        let failed = host.response(2, host_capture(session, channels));
        assert_eq!(failed["state"], "failed", "{failed}");
        let calls: Vec<_> = host
            .broker
            .calls
            .iter()
            .filter(|call| call.service == Resource::AudioCapture.service())
            .collect();
        assert!(calls.iter().any(|call| call.payload["action"] == "lookup"));
        assert!(calls.iter().any(|call| call.payload["action"] == "close"));
        let open = calls
            .iter()
            .find(|call| call.payload["action"] == "open")
            .unwrap();
        assert!(open.payload["channels"].is_null());
        assert!(open.payload["virtual_sink"].is_null());
        assert_eq!(open.payload["capture_format"]["channels"], channels);
        assert_eq!(host.broker.leases(), 0);
        assert_eq!(host.broker.live_components(), 0);
    }
}

#[test]
fn host_surround_requires_an_enabled_capture_only_endpoint_before_capture_opens() {
    for invalid in ["routing", "endpoint", "channels", "prepared_channels"] {
        let mut host = Harness::new();
        let session = host.response(1, preparation(6))["session"]
            .as_u64()
            .unwrap();
        let mut request = host_capture(session, 6);
        match invalid {
            "routing" => request["configuration"]["capture_sink_without_routing"] = json!(false),
            "endpoint" => request["configuration"]["audio_endpoint"] = json!(""),
            "prepared_channels" => request["configuration"]["channels"] = json!(8),
            _ => request["configuration"]["channels"] = json!(4),
        }
        assert_eq!(
            host.response(2, request)["code"],
            if invalid == "prepared_channels" {
                "audio_configuration_mismatch"
            } else {
                "invalid_virtual_sink_channels"
            },
            "{invalid}"
        );
        assert!(
            host.broker
                .calls
                .iter()
                .all(|call| call.service != Resource::AudioCapture.service())
        );
    }
}
