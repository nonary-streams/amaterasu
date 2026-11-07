use super::*;

#[test]
fn logical_identities_are_only_accepted_from_rtsp_and_are_echoed() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["lifecycle_session_target"] =
        json!({"kind":"session", "host_epoch":"epoch", "session_id":"session:1", "generation":1});
    assert_eq!(
        host.response(1, request.clone())["code"],
        "permission_denied"
    );
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    let response = host.response_from(2, "amaterasu.rtsp", request);
    assert_eq!(
        response["lifecycle_session_target"]["session_id"],
        "session:1"
    );
    assert_eq!(response["lifecycle_input_required"], false);
}

fn arm(id: u64) -> Value {
    let mut value = start(id);
    value["action"] = json!("start_when_authenticated");
    value
}

#[test]
fn shared_source_retirement_confirms_consumers_without_claiming_resource_release() {
    for subscriber in [false, true] {
        for valid in [false, true] {
            let mut host = Harness::new();
            host.pipeline
                .settings
                .allowed_principals
                .push("amaterasu.rtsp".into());
            host.broker.lifecycle_success = true;
            let source = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"source","generation":1});
            let target = if subscriber {
                json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"subscriber","generation":1})
            } else {
                source.clone()
            };
            let mut request = prepare();
            request["lifecycle_capture_mode"] = json!("external");
            request["lifecycle_session_target"] = json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1});
            let id = host.response_from(1, "amaterasu.rtsp", request)["session"]
                .as_u64()
                .unwrap();
            let mut configuration = arm(id);
            configuration["configuration"]["lifecycle_capture_mode"] = json!("external");
            configuration["configuration"]["lifecycle_target"] = target.clone();
            if subscriber {
                configuration["configuration"]["lifecycle_shared_source"] = source.clone();
            }
            assert_eq!(
                host.response_from(2, "amaterasu.rtsp", configuration)["state"],
                "armed"
            );
            host.idle();
            assert_eq!(host.pipeline.records[&id].state, State::Active);
            let mut proof = json!({"status":"succeeded","target":target,"plan_revision":null,
                "consumer_retired":true,"resources_released":false,"shared_retained":true,"shared_source":source});
            if !valid {
                proof["shared_source"]["generation"] = json!(2);
            }
            host.broker.lifecycle_retirement_override = Some(proof);
            let stopped =
                host.response_from(3, "amaterasu.rtsp", json!({"action":"stop","session":id}));
            assert_eq!(stopped["lifecycle_retired"], false);
            assert_eq!(stopped["lifecycle_resources_released"], false);
            if valid {
                assert_eq!(stopped["state"], "stopped", "{stopped}");
                assert_eq!(stopped["lifecycle_consumer_retired"], true);
                assert_eq!(stopped["lifecycle_shared_retained"], true);
                assert!(host.pipeline.records[&id].resources.is_empty());
                assert!(host.pipeline.records[&id].input_lease.is_none());
            } else {
                assert_eq!(stopped["state"], "cleanup_failed", "{stopped}");
                assert_eq!(stopped["lifecycle_consumer_retired"], false);
                assert_eq!(stopped["lifecycle_shared_retained"], false);
            }
        }
    }
}

fn arm_lifecycle(host: &mut Harness) -> (u64, Value) {
    let session_target =
        json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1});
    let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
    let mut request = prepare();
    request["lifecycle_session_target"] = session_target;
    let id = host.response_from(1, "amaterasu.rtsp", request)["session"]
        .as_u64()
        .unwrap();
    let mut start = arm(id);
    start["configuration"]["capture_provider"] = json!("pipewire");
    start["configuration"]["lifecycle_target"] = target.clone();
    start["configuration"]["lifecycle_candidate"] = json!({"capture_provider":"amaterasu.capture-pipewire/1","selector_schema":"amaterasu.kwin-wl-output/1","selector":{"output":"kwin:Virtual-12"},"resource":{"provider":"provider","resource_id":"r","incarnation":"i","revision":1,"execution_domain":"seat","mutation_domain":null},"mode":{"width":1280,"height":720,"refresh_millihz":60000,"hdr":false},"plan_revision":1,"geometry_revision":1,"generation":1});
    assert_eq!(
        host.response_from(2, "amaterasu.rtsp", start)["state"],
        "armed"
    );
    (id, target)
}

#[test]
fn processing_plan_uses_frozen_settings_and_rejects_source_mismatch() {
    for (enabled, source_hdr, output_hdr, bad_revision, omit_ranges, expected_open) in [
        (false, false, false, false, false, true),
        (false, false, true, false, false, false),
        (true, false, true, false, false, true),
        (true, true, true, false, false, false),
        (true, false, false, false, false, false),
        (true, false, true, true, false, false),
        (true, false, true, false, true, false),
    ] {
        let mut host = Harness::new();
        host.pipeline
            .settings
            .allowed_principals
            .push("amaterasu.rtsp".into());
        host.pipeline.settings.processing_plan = Some(crate::model::ProcessingPlanProvider {
            interface_minor: 0,
            provider: "example.gpu".into(),
            control_service: "example.processing-control/1".into(),
            processor_service: "example.gpu/1".into(),
            omit_disabled: false,
        });
        host.broker.lifecycle_success = true;
        host.broker.processing_plan_enabled = enabled;
        host.broker.processing_plan_source_hdr = source_hdr;
        host.broker.processing_plan_bad_revision = bad_revision;
        host.broker.processing_plan_omit_ranges = omit_ranges;
        let mut request = prepare();
        request["video"]["codecs"] = json!(["h264", "hevc"]);
        request["lifecycle_session_target"] =
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1});
        let id = host.response_from(1, "amaterasu.rtsp", request)["session"]
            .as_u64()
            .unwrap();
        let mut launch = arm(id);
        launch["configuration"]["capture_provider"] = json!("pipewire");
        launch["configuration"]["lifecycle_target"] = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
        launch["configuration"]["lifecycle_candidate"] = json!({"capture_provider":"amaterasu.capture-pipewire/1","selector_schema":"amaterasu.kwin-wl-output/1","selector":{"output":"kwin:Virtual-12"},"resource":{"provider":"provider","resource_id":"r","incarnation":"i","revision":1,"execution_domain":"seat","mutation_domain":null},"mode":{"width":1280,"height":720,"refresh_millihz":60000,"hdr":false},"plan_revision":1,"geometry_revision":1,"generation":1});
        launch["configuration"]["configuration"]["video_stream_format"] = json!({"codec":"hevc","bit_depth":10,"dynamic_range":if output_hdr {"hdr"} else {"sdr"}});
        assert_eq!(
            host.response_from(2, "amaterasu.rtsp", launch)["state"],
            "armed"
        );
        host.idle();
        assert!(host.broker.commands.iter().any(|command| matches!(command,
            Command::CallApi { service, method, interface_major: 1, interface_minor: 0, .. }
                if service == "example.processing-control/1" && method == "processing_intent"
        )));
        let open =
            host.broker.calls.iter().find(|call| {
                call.service == "amaterasu.video/1" && call.payload["action"] == "open"
            });
        assert_eq!(
            open.is_some(),
            expected_open,
            "enabled={enabled} source={source_hdr} output={output_hdr} bad_revision={bad_revision} omit_ranges={omit_ranges}"
        );
        if let Some(open) = open {
            assert_eq!(
                open.payload["configuration"]["gpu_processing"],
                json!({
                    "service":"example.gpu/1","enabled":enabled,"settings":{"test_control":42},
                    "source_hdr_required":false,
                    "live_settings":{"target":{"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1},"provider":"example.gpu","revision":"r1"}
                })
            );
        }
    }
}

#[test]
fn external_capture_receives_source_requirement_from_frozen_processing_plan() {
    for (minor, enabled, hdr_output, omit_disabled, selected) in [
        (0, false, false, false, true),
        (0, false, false, true, false),
        (0, false, true, true, true),
        (0, true, true, true, true),
        (1, false, true, true, true),
        (1, false, false, true, false),
    ] {
        let mut host = Harness::new();
        host.pipeline
            .settings
            .allowed_principals
            .push("amaterasu.rtsp".into());
        host.pipeline.settings.processing_plan = Some(crate::model::ProcessingPlanProvider {
            interface_minor: minor,
            provider: "example.gpu".into(),
            control_service: "example.processing-control/1".into(),
            processor_service: "example.gpu/1".into(),
            omit_disabled,
        });
        host.broker.lifecycle_success = true;
        host.broker.processing_plan_enabled = enabled;
        let mut request = prepare();
        request["video"]["codecs"] = json!(["h264", "hevc"]);
        request["lifecycle_capture_mode"] = json!("external");
        request["lifecycle_session_target"] =
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1});
        let id = host.response_from(1, "amaterasu.rtsp", request)["session"]
            .as_u64()
            .unwrap();
        let mut launch = arm(id);
        launch["configuration"]["lifecycle_capture_mode"] = json!("external");
        launch["configuration"]["lifecycle_target"] = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
        launch["configuration"]["configuration"]["video_stream_format"] = json!({"codec":"hevc","bit_depth":10,"dynamic_range":if hdr_output {"hdr"} else {"sdr"}});
        assert_eq!(
            host.response_from(2, "amaterasu.rtsp", launch)["state"],
            "armed"
        );
        host.idle();
        let open = host
            .broker
            .calls
            .iter()
            .find(|call| call.service == "amaterasu.video/1" && call.payload["action"] == "open")
            .expect("video opened");
        let selection = open.payload["configuration"].get("gpu_processing");
        assert_eq!(selection.is_some(), selected);
        if let Some(selection) = selection {
            let prepared = enabled || (minor == 1 && hdr_output);
            assert_eq!(selection["enabled"], prepared);
            // With conversion disabled, the source must match the output.
            assert_eq!(
                selection["source_hdr_required"],
                if prepared { false } else { hdr_output }
            );
        }
    }
}

#[test]
fn selected_processing_rejects_legacy_stream_without_frozen_authority() {
    let mut host = Harness::new();
    host.pipeline.settings.processing_plan = Some(crate::model::ProcessingPlanProvider {
        interface_minor: 0,
        provider: "example.gpu".into(),
        control_service: "example.processing-control/1".into(),
        processor_service: "example.gpu/1".into(),
        omit_disabled: false,
    });
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let response = host.response(2, start(id));
    assert_eq!(response["code"], "processing_lifecycle_required");
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == "amaterasu.video/1")
    );
}

#[test]
fn processing_provider_context_reaches_video_without_changing_frozen_settings() {
    let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
    for enabled in [false, true] {
        for (context, accepted) in [
            (json!({"foreground_target":target}), true),
            (
                json!({"replacement_context":{"stream":"other-provider","generation":2}}),
                true,
            ),
            (json!({"test_control":43}), false),
            (
                json!({"context":"x".repeat(amaterasu_module_sdk::extension_api::MAX_PROCESSING_CONFIGURATION_BYTES)}),
                false,
            ),
        ] {
            let mut host = Harness::new();
            host.pipeline
                .settings
                .allowed_principals
                .push("amaterasu.rtsp".into());
            host.pipeline.settings.processing_plan = Some(crate::model::ProcessingPlanProvider {
                interface_minor: 0,
                provider: "example.gpu".into(),
                control_service: "example.processing-control/1".into(),
                processor_service: "example.gpu/1".into(),
                omit_disabled: false,
            });
            host.broker.lifecycle_success = true;
            host.broker.processing_plan_enabled = enabled;
            host.broker.processing_plan_context = Some(context.clone());
            let mut request = prepare();
            request["video"]["codecs"] = json!(["h264", "hevc"]);
            request["lifecycle_capture_mode"] = json!("external");
            request["lifecycle_session_target"] = json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1});
            let id = host.response_from(1, "amaterasu.rtsp", request)["session"]
                .as_u64()
                .unwrap();
            let mut launch = arm(id);
            launch["configuration"]["lifecycle_capture_mode"] = json!("external");
            launch["configuration"]["lifecycle_target"] = target.clone();
            launch["configuration"]["configuration"]["video_stream_format"] = json!({"codec":"hevc","bit_depth":10,"dynamic_range":if enabled {"hdr"} else {"sdr"}});
            assert_eq!(
                host.response_from(2, "amaterasu.rtsp", launch)["state"],
                "armed"
            );
            host.idle();
            let open = host.broker.calls.iter().find(|call| {
                call.service == "amaterasu.video/1" && call.payload["action"] == "open"
            });
            assert_eq!(open.is_some(), accepted, "enabled={enabled}");
            if let Some(open) = open {
                let mut expected = json!({"test_control":42});
                expected
                    .as_object_mut()
                    .unwrap()
                    .extend(context.as_object().unwrap().clone());
                assert_eq!(
                    open.payload["configuration"]["gpu_processing"]["settings"],
                    expected
                );
            }
        }
    }
}

#[test]
fn external_lifecycle_keeps_capture_and_input_and_retires_after_native_close() {
    for input in [false, true] {
        let mut host = Harness::new();
        host.pipeline
            .settings
            .allowed_principals
            .push("amaterasu.rtsp".into());
        host.pipeline.settings.enable_input = input;
        host.broker.lifecycle_success = true;
        let mut request = prepare();
        request["lifecycle_capture_mode"] = json!("external");
        request["lifecycle_session_target"] =
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1});
        let id = host.response_from(1, "amaterasu.rtsp", request)["session"]
            .as_u64()
            .unwrap();
        let mut configuration = arm(id);
        configuration["configuration"]["lifecycle_capture_mode"] = json!("external");
        configuration["configuration"]["lifecycle_target"] = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
        assert_eq!(
            host.response_from(2, "amaterasu.rtsp", configuration)["state"],
            "armed"
        );
        host.idle();
        assert_eq!(host.pipeline.records[&id].state, State::Active);
        let ticks = host.plane.borrow().ticks;
        host.idle();
        assert!(host.plane.borrow().ticks > ticks);
        let calls = &host.broker.calls;
        assert!(
            calls
                .iter()
                .any(|call| call.payload["operation"] == "ack_stream")
        );
        assert!(!calls.iter().any(|call| matches!(
            call.payload["operation"].as_str(),
            Some("validate_candidate" | "ack_binding" | "binding_validate" | "issue_input_binding")
        )));
        assert!(
            !calls
                .iter()
                .any(|call| call.payload["action"] == "observe_capture")
        );
        let stopped =
            host.response_from(3, "amaterasu.rtsp", json!({"action":"stop","session":id}));
        assert_eq!(stopped["lifecycle_retired"], true, "{stopped}");
        let calls = &host.broker.calls;
        let close = calls
            .iter()
            .position(|call| {
                call.service == "amaterasu.video/1" && call.payload["action"] == "close"
            })
            .unwrap();
        let retire = calls
            .iter()
            .position(|call| call.payload["operation"] == "retire_stream")
            .unwrap();
        assert!(close < retire);
    }
}

#[test]
fn lifecycle_capture_waits_for_authority_and_retires_after_close() {
    let mut host = Harness::new();
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    host.broker.lifecycle_success = true;
    host.broker.lifecycle_pending_once = true;
    let (id, target) = arm_lifecycle(&mut host);
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Active);
    let calls = &host.broker.calls;
    let index = |service: &str, key: &str, action: &str| {
        calls
            .iter()
            .position(|call| call.service == service && call.payload[key] == action)
            .unwrap()
    };
    assert!(
        index("amaterasu.session-lifecycle/1", "operation", "lookup")
            < index("amaterasu.video/1", "action", "open")
    );
    assert!(
        index("amaterasu.video/1", "action", "observe_capture")
            < index("amaterasu.session-lifecycle/1", "operation", "ack_binding")
    );
    let ticks_before_stop = host.plane.borrow().ticks;
    let stopped = host.response_from(3, "amaterasu.rtsp", json!({"action":"stop","session":id}));
    // The harness pumps once before admitting the queued stop request.
    assert!(host.plane.borrow().ticks <= ticks_before_stop + 1);
    assert_eq!(stopped["lifecycle_retired"], true);
    assert_eq!(stopped["lifecycle_target"], target);
    let calls = &host.broker.calls;
    let position = |service: &str, key: &str, action: &str| {
        calls
            .iter()
            .position(|call| call.service == service && call.payload[key] == action)
            .unwrap()
    };
    assert!(
        position("amaterasu.session-lifecycle/1", "operation", "pause_stream")
            < position("amaterasu.video/1", "action", "close")
    );
    assert!(
        position("amaterasu.video/1", "action", "close")
            < position(
                "amaterasu.session-lifecycle/1",
                "operation",
                "retire_stream"
            )
    );
    assert!(
        !calls
            .iter()
            .any(|call| call.service == "amaterasu.video/1" && call.payload["action"] == "drain")
    );
}

#[test]
fn lifecycle_rejection_blocks_capture_and_unconfirmed_retirement() {
    let mut host = Harness::new();
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    host.broker.lifecycle_success = true;
    host.broker.lifecycle_reject_operation = Some("validate_candidate".into());
    let (id, _) = arm_lifecycle(&mut host);
    host.idle();
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == "amaterasu.video/1" && call.payload["action"] == "open")
    );
    assert_ne!(host.pipeline.records[&id].state, State::Active);

    let mut host = Harness::new();
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    host.broker.lifecycle_success = true;
    host.broker.lifecycle_omit_consumer_confirmation = true;
    let (id, _) = arm_lifecycle(&mut host);
    host.idle();
    assert_ne!(host.pipeline.records[&id].state, State::Active);
    assert_eq!(
        host.pipeline.records[&id].error,
        Some("lifecycle_consumers_unconfirmed")
    );
}

#[test]
fn lifecycle_fence_rejection_cannot_report_retired() {
    let mut host = Harness::new();
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    host.broker.lifecycle_success = true;
    let (id, _) = arm_lifecycle(&mut host);
    host.idle();
    host.broker.lifecycle_reject_operation = Some("pause_stream".into());
    let stopped = host.response_from(3, "amaterasu.rtsp", json!({"action":"stop","session":id}));
    assert_eq!(stopped["state"], "cleanup_failed");
    assert_eq!(stopped["lifecycle_retired"], false);
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.payload["operation"] == "retire_stream")
    );
}

#[test]
fn changed_active_observation_stops_delivery_and_reports_invalidation() {
    let mut host = Harness::new();
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    host.broker.lifecycle_success = true;
    let (id, _) = arm_lifecycle(&mut host);
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Active);
    host.broker.lifecycle_observation_override = Some(
        json!({"protocol":"wayland-wl-output","registry_global":5,"output_name":"Virtual-12"}),
    );
    host.idle();
    assert_ne!(host.pipeline.records[&id].state, State::Active);
    assert!(
        host.broker
            .calls
            .iter()
            .any(|call| call.payload["operation"] == "capture_invalidated")
    );
}

#[test]
fn unavailable_active_validation_stops_delivery() {
    let mut host = Harness::new();
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    host.broker.lifecycle_success = true;
    let (id, _) = arm_lifecycle(&mut host);
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Active);
    host.broker.lifecycle_reject_operation = Some("binding_validate".into());
    host.idle();
    assert_ne!(host.pipeline.records[&id].state, State::Active);
}

#[test]
fn lifecycle_stream_rejects_input_when_authority_denies_binding() {
    let mut host = Harness::new();
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    host.pipeline.settings.enable_input = true;
    host.broker.lifecycle_success = true;
    host.broker.lifecycle_reject_operation = Some("issue_input_binding".into());
    let (id, _) = arm_lifecycle(&mut host);
    host.idle();
    assert_ne!(host.pipeline.records[&id].state, State::Active);
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == "amaterasu.input/1" && call.payload["action"] == "acquire")
    );
}

#[test]
fn staged_bound_input_uses_token_and_monotonic_batch_ids() {
    let mut host = Harness::new();
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    host.pipeline.settings.enable_input = true;
    host.broker.lifecycle_success = true;
    let (id, _) = arm_lifecycle(&mut host);
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Active);
    host.pipeline
        .records
        .get_mut(&id)
        .unwrap()
        .input_events
        .push_back(json!({"kind":"key","key":30,"down":true}));
    host.idle();
    host.pipeline
        .records
        .get_mut(&id)
        .unwrap()
        .input_events
        .push_back(json!({"kind":"key","key":30,"down":false}));
    host.idle();
    let batches: Vec<_> = host
        .broker
        .calls
        .iter()
        .filter(|call| call.service == "amaterasu.input/1" && call.payload["action"] == "batch")
        .map(|call| call.payload.clone())
        .collect();
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0]["operation_id"], "epoch-1:1");
    assert_eq!(batches[1]["operation_id"], "epoch-1:2");
    assert_eq!(batches[0]["input_binding"], "binding-token");
}

#[test]
fn discovered_media_waits_for_both_lanes_without_opening_capture() {
    let mut host = Harness::new();
    let prepared = host.response(1, prepare_discovery());
    let id = prepared["session"].as_u64().unwrap();
    let deadline = host.pipeline.records[&id].prepared_until;
    assert_eq!(prepared["media_ready"], false);
    assert_eq!(prepared["peer_discovery"], true);
    assert!(prepared["video_peer_port"].is_null());
    assert!(prepared["audio_peer_port"].is_null());
    assert!(!prepared.to_string().contains("0123456789abcdef"));
    assert!(!prepared.to_string().contains("fedcba9876543210"));
    let media_open = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == Resource::Media.service())
        .expect("media open");
    assert!(media_open.payload.get("video_port").is_none());
    assert!(media_open.payload.get("audio_port").is_none());
    assert_eq!(
        media_open.payload["discovery"]["video_ping_payload"],
        "0123456789abcdef"
    );

    assert_eq!(host.response(2, arm(id))["state"], "armed");
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Armed);
    assert_eq!(host.pipeline.records[&id].prepared_until, deadline);
    assert_eq!(host.plane.borrow().ticks, 0);
    assert!(!host.broker.calls.iter().any(|call| {
        call.service == Resource::Video.service()
            || call.service == Resource::AudioCapture.service()
    }));

    host.broker.discovered_audio_port = Some(51_002);
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Armed);
    assert_eq!(host.plane.borrow().ticks, 0);

    host.broker.discovered_video_port = Some(51_001);
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Active);
    assert_eq!(host.pipeline.records[&id].video_peer_port, Some(51_001));
    assert_eq!(host.pipeline.records[&id].audio_peer_port, Some(51_002));
}

#[test]
fn ready_but_failed_media_rolls_back_before_capture() {
    let mut host = Harness::new();
    host.broker.discovered_video_port = Some(51_001);
    host.broker.discovered_audio_port = Some(51_002);
    host.broker.media_failure = true;
    let id = host.response(1, prepare_discovery())["session"]
        .as_u64()
        .unwrap();
    assert_eq!(host.response(2, arm(id))["state"], "armed");
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Failed);
    assert_eq!(host.pipeline.records[&id].error, Some("media_unavailable"));
    assert_eq!(host.plane.borrow().ticks, 0);
    assert!(!host.broker.calls.iter().any(|call| {
        call.service == Resource::Video.service()
            || call.service == Resource::AudioCapture.service()
    }));
}

#[test]
fn discovery_timeout_and_stop_with_one_lane_release_without_capture() {
    let mut timed_out = Harness::new();
    let id = timed_out.response(1, prepare_discovery())["session"]
        .as_u64()
        .unwrap();
    assert_eq!(timed_out.response(2, arm(id))["state"], "armed");
    timed_out.now = timed_out.pipeline.records[&id].prepared_until;
    timed_out.idle();
    assert_eq!(timed_out.pipeline.records[&id].state, State::Failed);
    assert_eq!(
        timed_out.pipeline.records[&id].error,
        Some("prepare_expired")
    );
    assert_eq!(timed_out.broker.live_components(), 0);
    assert_eq!(timed_out.plane.borrow().ticks, 0);

    let mut stopped = Harness::new();
    let id = stopped.response(1, prepare_discovery())["session"]
        .as_u64()
        .unwrap();
    assert_eq!(stopped.response(2, arm(id))["state"], "armed");
    stopped.broker.discovered_audio_port = Some(51_002);
    stopped.idle();
    assert_eq!(stopped.pipeline.records[&id].state, State::Armed);
    assert_eq!(
        stopped.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    assert_eq!(stopped.broker.live_components(), 0);
    assert_eq!(stopped.plane.borrow().ticks, 0);
}

#[test]
fn armed_wait_has_no_hardware_and_authentication_runs_existing_start_once() {
    let mut host = Harness::new();
    host.broker.authenticated = false;
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let deadline = host.pipeline.records[&id].prepared_until;
    assert_eq!(host.response(2, arm(id))["state"], "armed");
    host.broker.cancel_parent(2);
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Armed);
    assert_eq!(host.plane.borrow().ticks, 0);
    assert_eq!(host.broker.live_components(), 3);
    assert_eq!(host.pipeline.records[&id].prepared_until, deadline);
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == Resource::Video.service())
    );
    host.broker.authenticated = true;
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Active);
    assert_eq!(host.broker.leases(), 2);
    assert_eq!(
        host.broker
            .calls
            .iter()
            .filter(|call| call.service == Resource::Video.service()
                && call.payload["action"] == "open")
            .count(),
        1
    );
    assert!(
        host.broker
            .calls
            .iter()
            .filter(|call| matches!(
                call.payload["action"].as_str(),
                Some("acquire" | "activate")
            ) || call.service == Resource::Video.service()
                || call.service == Resource::AudioCapture.service())
            .all(|call| call.parent.is_none())
    );
    assert_eq!(host.response(3, arm(id))["state"], "active");
}

#[test]
fn armed_timeout_does_not_extend_prepare_and_rolls_back_without_capture() {
    let mut host = Harness::new();
    host.broker.authenticated = false;
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let deadline = host.pipeline.records[&id].prepared_until;
    host.now = deadline - 10;
    assert_eq!(host.response(2, arm(id))["state"], "armed");
    assert_eq!(host.response(3, arm(id))["state"], "armed");
    assert_eq!(host.pipeline.records[&id].prepared_until, deadline);
    host.now = deadline;
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Failed);
    assert_eq!(host.pipeline.records[&id].error, Some("prepare_expired"));
    assert_eq!(host.broker.live_components(), 0);
    assert_eq!(host.broker.leases(), 0);
    assert_eq!(host.plane.borrow().ticks, 0);
}

#[test]
fn stop_before_authentication_cancels_armed_lifecycle() {
    let mut host = Harness::new();
    host.broker.authenticated = false;
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    host.response(2, arm(id));
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    host.broker.authenticated = true;
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Stopped);
    assert_eq!(host.broker.live_components(), 0);
    assert_eq!(host.plane.borrow().ticks, 0);
    assert_eq!(host.response(4, arm(id))["state"], "stopped");
}

#[test]
fn arm_owner_validation_conflicts_and_ordinary_start_semantics_are_preserved() {
    let mut host = Harness::new();
    host.broker.authenticated = false;
    host.pipeline
        .settings
        .allowed_principals
        .push("foreign".into());
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(
        host.response(2, start(id))["code"],
        "control_not_authenticated"
    );
    host.broker.enqueue(3, "foreign", arm(id));
    host.idle();
    assert_eq!(host.broker.replies[&3]["code"], "session_not_found");
    let mut invalid = arm(id);
    invalid["configuration"]["configuration"]["width"] = json!(1);
    assert_eq!(
        host.response(4, invalid)["code"],
        "invalid_start_configuration"
    );
    host.response(5, arm(id));
    let mut changed = arm(id);
    changed["configuration"]["encoder"] = json!(1);
    assert_eq!(host.response(6, changed)["code"], "start_conflict");
    assert_eq!(host.response(7, start(id))["code"], "invalid_state");
    let mut unknown = arm(id);
    unknown["assume_authenticated"] = json!(true);
    assert_eq!(host.response(8, unknown)["code"], "invalid_request");
}

#[test]
fn authenticated_start_accepts_matching_encryption_snapshot_and_rejects_mismatch() {
    let mut host = Harness::new();
    let mut preparation = prepare();
    preparation["encrypt_video"] = json!(false);
    preparation["encrypt_audio"] = json!(false);
    let id = host.response(1, preparation)["session"].as_u64().unwrap();

    let mut mismatched = arm(id);
    mismatched["configuration"]["encrypt_video"] = json!(false);
    mismatched["configuration"]["encrypt_audio"] = json!(true);
    assert_eq!(
        host.response(2, mismatched)["code"],
        "encryption_configuration_mismatch"
    );
    assert_eq!(host.pipeline.records[&id].state, State::Prepared);
    assert!(host.pipeline.records[&id].start.is_none());

    let mut matching = arm(id);
    matching["configuration"]["encrypt_video"] = json!(false);
    matching["configuration"]["encrypt_audio"] = json!(false);
    assert_eq!(host.response(3, matching)["state"], "armed");
}

#[test]
fn cancelled_or_lost_initial_arm_completion_rolls_back_but_retry_does_not() {
    for cancelled in [true, false] {
        let mut host = Harness::new();
        host.broker.authenticated = false;
        let id = host.response(1, prepare())["session"].as_u64().unwrap();
        host.broker.enqueue(2, "owner", arm(id));
        if cancelled {
            host.broker.cancel_parent(2);
        } else {
            host.broker.fail_completion = true;
        }
        host.idle();
        assert_eq!(host.pipeline.records[&id].state, State::Cancelled);
        assert_eq!(host.broker.live_components(), 0);
        assert!(!host.pipeline.records[&id].armed_accepted);
    }
    let mut host = Harness::new();
    host.broker.authenticated = false;
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    host.response(2, arm(id));
    host.broker.fail_completion = true;
    host.broker.enqueue(3, "owner", arm(id));
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Armed);
    host.broker.authenticated = true;
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Active);
}

#[test]
fn autonomous_capture_failure_uses_existing_confirmed_cleanup() {
    let mut host = Harness::new();
    host.broker.authenticated = false;
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    host.response(2, arm(id));
    host.broker.format_rate = 44100;
    host.broker.authenticated = true;
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Failed);
    assert_eq!(host.broker.live_components(), 0);
    assert_eq!(host.broker.leases(), 0);
    assert_eq!(
        host.pipeline.records[&id].error,
        Some("unsupported_audio_format")
    );
}

fn prepared_lifecycle_for_cleanup(external: bool) -> (Harness, u64, Value) {
    let mut host = Harness::new();
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    host.broker.lifecycle_success = true;
    host.broker.authenticated = false;
    let mut request = prepare();
    request["lifecycle_session_target"] =
        json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1});
    if external {
        request["lifecycle_capture_mode"] = json!("external");
    }
    let id = host.response_from(1, "amaterasu.rtsp", request)["session"]
        .as_u64()
        .unwrap();
    let mut configuration = start(id)["configuration"].clone();
    configuration["lifecycle_target"] = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
    if external {
        configuration["lifecycle_capture_mode"] = json!("external");
    } else {
        configuration["capture_provider"] = json!("pipewire");
        configuration["lifecycle_candidate"] = json!({"capture_provider":"amaterasu.capture-pipewire/1","selector_schema":"amaterasu.kwin-wl-output/1","selector":{"output":"kwin:Virtual-12"},"resource":{"provider":"provider","resource_id":"r","incarnation":"i","revision":1,"execution_domain":"seat","mutation_domain":null},"mode":{"width":1280,"height":720,"refresh_millihz":60000,"hdr":false},"plan_revision":1,"geometry_revision":1,"generation":1});
    }
    (host, id, configuration)
}

#[test]
fn stop_retires_a_prepared_lifecycle_without_starting_native_capture() {
    for external in [false, true] {
        for already_stopped in [false, true] {
            let (mut host, id, configuration) = prepared_lifecycle_for_cleanup(external);
            if already_stopped {
                assert_eq!(
                    host.response_from(2, "amaterasu.rtsp", json!({"action":"stop","session":id}))
                        ["state"],
                    "stopped"
                );
            }
            let mut wrong_configuration = configuration.clone();
            wrong_configuration["lifecycle_target"]["session_id"] = json!("another-session");
            assert_eq!(host.response_from(3, "amaterasu.rtsp", json!({"action":"stop","session":id,"lifecycle_configuration":wrong_configuration}))["code"], "invalid_lifecycle_identity");
            let stopped = host.response_from(
                4,
                "amaterasu.rtsp",
                json!({"action":"stop","session":id,"lifecycle_configuration":configuration}),
            );
            assert_eq!(
                stopped["state"], "stopped",
                "external={external} already_stopped={already_stopped}: {stopped}"
            );
            assert_eq!(stopped["lifecycle_retired"], true, "{stopped}");
            let pause = host
                .broker
                .calls
                .iter()
                .position(|call| call.payload["operation"] == "pause_stream")
                .unwrap();
            let journal = host.broker.calls.iter().position(|call| call.service == "amaterasu.state/1" && call.payload["action"] == "set" && call.payload["value"]["entries"][id.to_string()]["lifecycle_recovery"]["stream_target"] == configuration["lifecycle_target"]).unwrap();
            assert!(
                journal < pause,
                "admitted target must be durable before fencing"
            );
            assert!(
                host.broker
                    .calls
                    .iter()
                    .any(|call| call.payload["operation"] == "retire_stream")
            );
            assert!(
                !host
                    .broker
                    .calls
                    .iter()
                    .any(|call| call.payload["action"] == "open"
                        && matches!(
                            call.service.as_str(),
                            "amaterasu.video/1" | "amaterasu.audio/1"
                        ))
            );
            assert!(
                host.broker.state_value.as_ref().unwrap()["entries"]
                    .as_object()
                    .unwrap()
                    .is_empty()
            );
        }
    }
}

#[test]
fn cleanup_binding_adoption_waits_for_durable_journal_confirmation() {
    let (mut host, id, configuration) = prepared_lifecycle_for_cleanup(false);
    host.broker.state_fail_set = true;
    let stopped = host.response_from(
        2,
        "amaterasu.rtsp",
        json!({"action":"stop","session":id,"lifecycle_configuration":configuration}),
    );
    assert_eq!(stopped["state"], "cleanup_failed", "{stopped}");
    assert_eq!(stopped["cleanup_error"], "lifecycle_journal_write_failed");
    let writes = host
        .broker
        .calls
        .iter()
        .filter(|call| call.service == "amaterasu.state/1" && call.payload["action"] == "set")
        .count();
    host.broker.state_fail_set = false;
    let retried = host.response_from(3, "amaterasu.rtsp", json!({"action":"stop","session":id}));
    assert_eq!(retried["state"], "cleanup_failed", "{retried}");
    assert_eq!(
        host.broker
            .calls
            .iter()
            .filter(|call| call.service == "amaterasu.state/1" && call.payload["action"] == "set")
            .count(),
        writes,
        "unconfirmed journal must be reconciled before another operation can race it"
    );
    assert!(!host.broker.calls.iter().any(|call| matches!(
        call.payload["operation"].as_str(),
        Some("pause_stream" | "retire_stream")
    )));
    assert!(
        host.broker.state_value.as_ref().unwrap()["entries"][id.to_string()]["lifecycle_recovery"]
            ["stream_target"]
            .is_null()
    );
}
