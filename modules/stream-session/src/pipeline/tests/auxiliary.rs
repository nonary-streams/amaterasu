use super::*;

fn assert_no_display_calls(host: &Harness) {
    assert!(host.broker.calls.iter().all(|call| !matches!(
        call.service.as_str(),
        "amaterasu.display/1" | "amaterasu.virtual-display/1"
    )));
}

#[test]
fn independent_capture_without_audio_only_owns_its_input_and_transport() {
    for (provider, output) in [("black", "black"), ("replacement_black", "blank-frame")] {
        let mut host = Harness::new();
        host.pipeline.settings.enable_input = true;
        host.pipeline.settings.input_lease_ttl_ms = 1_000;
        let mut request = prepare();
        request["stream_audio"] = json!(false);
        let prepared = host.response(1, request);
        assert_eq!(prepared["state"], "prepared", "{prepared}");
        let session = prepared["session"].as_u64().unwrap();
        let mut configuration = start(session);
        configuration["configuration"]["capture_provider"] = json!(provider);
        configuration["configuration"]["output"] = json!(output);
        configuration["configuration"]["audio_endpoint"] = json!("");
        configuration["configuration"]["capture_sink_without_routing"] = json!(true);
        let active = host.response(2, configuration);
        assert_eq!(active["state"], "active", "{active}");
        let video = host
            .broker
            .calls
            .iter()
            .find(|call| {
                call.service == Resource::Video.service() && call.payload["action"] == "open"
            })
            .expect("selected external capture reaches video");
        assert_eq!(video.payload["capture_provider"], provider);
        assert_eq!(video.payload["output"], output);
        assert!(host.broker.input_lease.is_some());
        assert_eq!(host.broker.leases(), 1);
        assert_no_display_calls(&host);
        assert!(host.broker.calls.iter().all(|call| !matches!(
            call.service.as_str(),
            "amaterasu.audio/1" | "amaterasu.audio-encode/1"
        )));

        let stopped = host.response(3, json!({"action":"stop","session":session}));
        assert_eq!(stopped["state"], "stopped", "{stopped}");
        assert_eq!(host.broker.leases(), 0);
        assert_eq!(host.broker.live_components(), 0);
        assert!(host.broker.input_lease.is_none());
        assert_no_display_calls(&host);
    }
}

#[test]
fn borrowed_exact_output_never_creates_or_releases_the_retained_monitor() {
    for stream_audio in [false, true] {
        let mut host = Harness::new();
        host.pipeline.settings.enable_input = true;
        host.pipeline.settings.input_lease_ttl_ms = 1_000;
        let monitor = json!({"identity":"retained-client-monitor","state":"ready"});
        host.broker
            .virtual_displays
            .insert("retained-client-monitor".into(), monitor.clone());
        let mut request = prepare();
        request["stream_audio"] = json!(stream_audio);
        let session = host.response(1, request)["session"].as_u64().unwrap();
        let mut configuration = start(session);
        configuration["configuration"]["capture_provider"] = json!("virtual_screen");
        configuration["configuration"]["output"] = json!("kwin:Virtual-17");
        configuration["configuration"]["host_audio_enabled"] = json!(true);
        configuration["configuration"]["capture_sink_without_routing"] = json!(true);
        if !stream_audio {
            configuration["configuration"]["audio_endpoint"] = json!("");
        }
        let active = host.response(2, configuration);
        assert_eq!(active["state"], "active", "{active}");
        let video = host
            .broker
            .calls
            .iter()
            .find(|call| {
                call.service == Resource::Video.service() && call.payload["action"] == "open"
            })
            .expect("exact retained capture reaches video");
        assert_eq!(video.payload["capture_provider"], "virtual_screen");
        assert_eq!(video.payload["output"], "kwin:Virtual-17");
        assert_no_display_calls(&host);
        let audio = host.broker.calls.iter().find(|call| {
            call.service == Resource::AudioCapture.service() && call.payload["action"] == "open"
        });
        assert_eq!(audio.is_some(), stream_audio);
        if let Some(audio) = audio {
            assert_eq!(audio.payload["host_audio_enabled"], true);
            assert_eq!(audio.payload["capture_sink_without_routing"], true);
        }
        let stopped = host.response(3, json!({"action":"stop","session":session}));
        assert_eq!(stopped["state"], "stopped", "{stopped}");
        assert_eq!(host.broker.leases(), 0);
        assert_no_display_calls(&host);
        assert_eq!(
            host.broker.virtual_displays.get("retained-client-monitor"),
            Some(&monitor)
        );
    }
}
