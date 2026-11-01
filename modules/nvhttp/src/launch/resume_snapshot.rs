//! Prepare a fresh stream snapshot without mutating the retained application.
use super::{Controller, LaunchRequest, PeerIdentity, Record, workflow};

/// A validated candidate. Application identity and lifecycle ownership remain
/// in the retained record; no provider effects occur during construction.
pub(super) struct ResumeSnapshot {
    stream: Record,
}

impl ResumeSnapshot {
    pub(super) fn legacy_display_conflict(&self) -> bool {
        self.stream.virtual_display_intent.is_some()
            || self
                .stream
                .display_policy_snapshot
                .as_ref()
                .and_then(|snapshot| {
                    snapshot["effective_overrides"]["dd_configuration_option"].as_str()
                })
                .is_some_and(|option| option != "disabled")
    }
    pub(super) fn build(
        controller: &Controller,
        retained: &Record,
        request: LaunchRequest,
        peer: PeerIdentity,
        connect_data: u32,
        global_proton_dualsense_compatibility: bool,
    ) -> Result<Self, &'static str> {
        let config = controller.config.as_ref().ok_or("streaming_disabled")?;
        if !controller.output_settings_ready || connect_data == 0 {
            return Err("stream_settings_unavailable");
        }
        if controller.global_display_revision.is_none() {
            return Err("display_policy_unavailable");
        }
        if !config.profile.accepts(&request) {
            return Err("invalid_streaming_request");
        }
        let output = controller
            .configured_output_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or(&config.capture.output)
            .to_owned();
        let endpoint = controller
            .configured_audio_sink
            .as_deref()
            .unwrap_or(&config.capture.audio_endpoint)
            .to_owned();
        let mode = peer.client_capture.unwrap_or(controller.configured_capture);
        let (provider, constant_rate, routed_output) =
            super::resolve_capture_route(mode, config.capture.capture_provider.clone(), &output);
        let device_name = request
            .device_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or(&controller.device_name)
            .to_owned();
        let mut stream = Record::new(
            retained.id,
            peer.clone(),
            request,
            device_name,
            retained.application_operation.clone(),
            retained.rtsp_operation.clone(),
            connect_data,
            controller
                .global_fec_percentage
                .unwrap_or(config.profile.fec_percentage),
            controller.global_stream_audio,
            controller.global_video_encryption_policy,
            controller.global_audio_sink_capture_only,
            controller.configured_audio_sink.clone(),
            controller.configured_virtual_sink.clone(),
            controller.global_preparation.clone(),
            controller.legacy_global_preparation.clone(),
            controller.global_frame_limiter_settings.clone(),
            peer.client_frame_limiter_settings.clone(),
            controller.global_ping_timeout_ms,
            controller.global_video_max_batch_size_kb,
            controller.global_max_bitrate_kbps,
            controller.global_qp,
            controller.global_minimum_fps_target,
            controller.global_wgc_pacing_smoothing,
            routed_output,
            config.capture.capture_provider.clone(),
            controller.configured_capture,
            endpoint,
            config.capture.audio_endpoint.clone(),
            provider,
            constant_rate,
            0,
            retained.deadline,
        );
        stream.desktop = retained.desktop;
        stream.resumed = true;
        stream.requested_capture_output = output;
        stream.encoder_snapshot = controller.encoder_snapshot.clone();
        stream.selected_encoder = config.capture.encoder.clone();
        stream.global_software_encoder = controller.software_encoder.clone();
        stream.global_nvenc_settings = controller.nvenc_settings.clone();
        stream.global_nvenc_spatial_aq = controller.nvenc_spatial_aq;
        stream.global_display_revision = controller.global_display_revision;
        stream.global_display_policy = controller.global_display_policy.clone();
        stream.global_proton_dualsense_compatibility = Some(global_proton_dualsense_compatibility);
        stream.application_preflight_overrides = retained.application_preflight_overrides.clone();
        stream.application_frame_limiter_settings =
            retained.application_frame_limiter_settings.clone();
        stream.application_display_overrides = retained.application_display_overrides.clone();
        stream.application_display_settings = retained.application_display_settings.clone();
        stream.application_catalog_revision = retained.application_catalog_revision;
        stream.executable_path = retained.executable_path.clone();
        let application = stream
            .application_preflight_overrides
            .clone()
            .unwrap_or_default();
        let mut profile = config.profile.clone();
        workflow::project_application_policy(&mut stream, &application, &mut profile)?;
        stream.virtual_display_intent = workflow::virtual_display_intent(&stream)?;
        let selected_mode = peer
            .client_capture
            .or(application.capture)
            .unwrap_or(stream.global_capture_mode);
        let virtual_intent = stream.virtual_display_intent.is_some();
        workflow::route_capture(&mut stream, selected_mode, virtual_intent)?;
        stream.display_policy_snapshot = if stream.desktop {
            None
        } else {
            Some(workflow::display_policy_snapshot(&stream))
        };
        stream.frame_limiter_snapshot = Some(workflow::frame_limiter_launch_snapshot(&stream));
        Ok(Self { stream })
    }

    /// Apply only stream-specific values. The caller must revalidate lifecycle
    /// resources and obtain a fresh display-admission token before opening RTSP.
    pub(super) fn apply(self, retained: &mut Record) {
        let stream = self.stream;
        if retained.resume_previous_options.is_none() {
            retained.resume_previous_options = Some((
                retained.request.options.clone(),
                retained.request.entitlements.clone(),
            ));
        }
        macro_rules! move_fields { ($($field:ident),+ $(,)?) => { $(retained.$field = stream.$field;)+ }; }
        move_fields!(
            peer,
            request,
            physical_display_mode,
            device_name,
            connect_data,
            global_fec_percentage,
            global_stream_audio,
            video_encryption_policy,
            global_audio_sink_capture_only,
            global_configured_audio_sink,
            global_configured_virtual_sink,
            global_frame_limiter_settings,
            client_frame_limiter_settings,
            global_display_revision,
            global_display_policy,
            display_policy_snapshot,
            virtual_display_intent,
            frame_limiter_snapshot,
            resolved_virtual_sink,
            capture_sink_without_routing,
            scoped_back_button_timeout,
            scoped_controller,
            scoped_gamepad,
            scoped_keyboard,
            scoped_mouse,
            scoped_native_pen_touch,
            scoped_ds4_back_as_touchpad_click,
            scoped_motion_as_ds4,
            scoped_touchpad_as_ds4,
            scoped_ds5_inputtino_randomize_mac,
            scoped_high_resolution_scrolling,
            scoped_always_send_scancodes,
            scoped_key_rightalt_to_key_win,
            scoped_keybindings,
            scoped_key_repeat_delay,
            scoped_key_repeat_frequency,
            global_ping_timeout_ms,
            global_video_max_batch_size_kb,
            global_max_bitrate_kbps,
            global_qp,
            global_minimum_fps_target,
            effective_video_max_batch_size_kb,
            video_codec_offer,
            encoder_snapshot,
            effective_max_bitrate_kbps,
            effective_qp,
            effective_minimum_fps_target,
            global_wgc_pacing_smoothing,
            global_software_encoder,
            effective_software_encoder,
            global_nvenc_settings,
            effective_nvenc_settings,
            global_nvenc_spatial_aq,
            effective_nvenc_spatial_aq,
            selected_encoder,
            application_stream_audio,
            effective_stream_audio,
            capture_output,
            requested_capture_output,
            bootstrap_capture_provider,
            global_capture_mode,
            audio_endpoint,
            bootstrap_audio_endpoint,
            capture_provider,
            constant_rate,
            application_fec_percentage,
            effective_fec_percentage,
            global_proton_dualsense_compatibility,
            proton_dualsense_compatibility,
        );
        retained.display_topology_revision = None;
        retained.display_snapshot_configuration = None;
        retained.verified_display_id = None;
        retained.display_snapshot_ids.clear();
        retained.display_enumeration_offset = 0;
        retained.display_enumerated_ids.clear();
        retained.display_match_id = None;
        retained.display_match_inactive = false;
        retained.display_request = None;
        retained.display_admission_token = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::tests::{config, peer, request};
    use serde_json::json;
    use std::time::Instant;

    #[test]
    fn rebuild_updates_mode_and_live_policy_without_changing_application_ownership() {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        controller.set_global_display_policy(7, json!({}));
        assert!(controller.launch(1, request(1), peer(1), now).is_none());
        {
            let retained = controller.records.values_mut().next().unwrap();
            retained.global_proton_dualsense_compatibility = Some(true);
            retained.application_display_overrides = Some(json!({}));
            retained.application_launch = Some(91);
            retained.application_requested = true;
        }
        controller.global_qp = 22;
        controller.global_display_revision = Some(8);
        let mut next = request(2);
        next.width = 1280;
        next.height = 720;
        next.fps = 30;
        let original = controller.records.values().next().unwrap();
        let application_operation = original.application_operation.clone();
        let candidate =
            ResumeSnapshot::build(&controller, original, next, peer(1), 42, true).unwrap();
        let retained = controller.records.values_mut().next().unwrap();
        candidate.apply(retained);
        assert_eq!(retained.request.width, 1280);
        assert_eq!(retained.physical_display_mode.width, 1280);
        assert_eq!(retained.physical_display_mode.refresh_millihz, 30_000);
        assert_eq!(
            retained.frame_limiter_snapshot.as_ref().unwrap()["requested_stream_fps_millihz"],
            30_000
        );
        assert_eq!(retained.global_display_revision, Some(8));
        assert_eq!(retained.effective_qp, 22);
        assert_eq!(retained.application_operation, application_operation);
        assert_eq!(retained.application_launch, Some(91));
        assert!(retained.application_requested);
        assert!(retained.display_admission_token.is_none());
    }

    #[test]
    fn resume_replaces_only_the_validated_stream_option_snapshot() {
        use amaterasu_module_sdk::launch_options::{LaunchOption, LaunchOptions};

        let snapshot = |name: &str, value| {
            LaunchOptions::validate(
                vec![LaunchOption {
                    namespace: name.into(),
                    version: 1,
                    value,
                }],
                |namespace, version| {
                    matches!(
                        (namespace, version),
                        ("org.example.alpha", 1) | ("net.example.beta", 1)
                    )
                },
            )
            .unwrap()
        };
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        controller.set_global_display_policy(7, json!({}));
        let mut first = request(1);
        first.options = snapshot("org.example.alpha", json!(true));
        assert!(controller.launch(1, first, peer(1), now).is_none());
        controller.global_display_revision = Some(7);
        controller
            .records
            .values_mut()
            .next()
            .unwrap()
            .application_display_overrides = Some(json!({}));
        let original = controller.records.values().next().unwrap();
        assert_eq!(
            original.request.options.entries()[0].namespace,
            "org.example.alpha"
        );
        let mut next = request(2);
        next.options = snapshot("net.example.beta", json!({"value": 2}));
        let candidate =
            ResumeSnapshot::build(&controller, original, next, peer(1), 42, true).unwrap();
        let retained = controller.records.values_mut().next().unwrap();
        candidate.apply(retained);
        assert_eq!(
            retained.request.options.entries()[0].namespace,
            "net.example.beta"
        );
        let candidate = ResumeSnapshot::build(
            &controller,
            controller.records.values().next().unwrap(),
            request(3),
            peer(1),
            43,
            true,
        )
        .unwrap();
        candidate.apply(controller.records.values_mut().next().unwrap());
        assert!(
            controller
                .records
                .values()
                .next()
                .unwrap()
                .request
                .options
                .entries()
                .is_empty()
        );
    }

    #[test]
    fn authorization_failure_after_candidate_application_restores_prior_options() {
        use amaterasu_module_sdk::launch_options::{LaunchOption, LaunchOptions};
        use amaterasu_module_sdk::{Command, Response};
        use std::time::Duration;

        let snapshot = |name: &str| {
            LaunchOptions::validate(
                vec![LaunchOption {
                    namespace: name.into(),
                    version: 1,
                    value: json!(true),
                }],
                |_, _| true,
            )
            .unwrap()
        };
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        controller.set_global_display_policy(7, json!({}));
        let mut first = request(1);
        first.options = snapshot("org.example.alpha");
        first.entitlements = vec!["org.example/old".into()];
        assert!(controller.launch(1, first, peer(1), now).is_none());
        controller.global_display_revision = Some(7);
        controller
            .records
            .values_mut()
            .next()
            .unwrap()
            .application_display_overrides = Some(json!({}));
        let mut next = request(2);
        next.options = snapshot("net.example.beta");
        next.entitlements = vec!["net.example/new".into()];
        let record = controller.records.values().next().unwrap();
        let id = record.id;
        let candidate =
            ResumeSnapshot::build(&controller, record, next, peer(1), 42, true).unwrap();
        let record = controller.records.get_mut(&id).unwrap();
        candidate.apply(record);
        record.stage = super::super::record::Stage::AuthorizeResumeSnapshot;
        record.history_release_required = true;
        assert_eq!(
            record.request.options.entries()[0].namespace,
            "net.example.beta"
        );
        let mut job = super::super::workflow::Job {
            record: id,
            phase: super::super::workflow::Phase::AuthorizeResumeSnapshot,
            call: super::super::call::Call::new("test/1", json!({}), "test".into(), 5000),
            deadline: now + Duration::from_secs(10),
        };
        let mut replies = Vec::new();
        for _ in 0..2 {
            controller.advance(
                &mut job,
                now,
                &mut |command| match command {
                    Command::Request { .. } => Ok(Response::Accepted { id: 7 }),
                    Command::TakeResult { .. } => Ok(Response::Result {
                        payload: json!({"ok": false}),
                    }),
                    Command::ReleaseResult { .. } => Ok(Response::Ok),
                    _ => panic!("unexpected broker command"),
                },
                &mut replies,
            );
            if let Some(next) = controller.job.take() {
                job = next;
            }
        }
        let record = &controller.records[&id];
        assert_eq!(
            record.request.options.entries()[0].namespace,
            "org.example.alpha"
        );
        assert_eq!(record.request.entitlements, ["org.example/old"]);
        assert!(record.resume_previous_options.is_none());
        assert_eq!(
            record.stage,
            super::super::record::Stage::ReleaseRejectedResumeHistory
        );
        assert_eq!(replies.len(), 1);
    }

    #[test]
    fn invalid_resume_candidate_leaves_retained_record_unchanged() {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        controller.set_global_display_policy(7, json!({}));
        let mut first = request(1);
        first.options = amaterasu_module_sdk::launch_options::LaunchOptions::validate(
            vec![amaterasu_module_sdk::launch_options::LaunchOption {
                namespace: "org.example.alpha".into(),
                version: 1,
                value: json!(true),
            }],
            |_, _| true,
        )
        .unwrap();
        assert!(controller.launch(1, first, peer(1), now).is_none());
        controller.global_display_revision = Some(7);
        let original = controller.records.values().next().unwrap();
        let width = original.request.width;
        let options = original.request.options.clone();
        let mut invalid = request(2);
        invalid.width = 50_000;
        assert!(ResumeSnapshot::build(&controller, original, invalid, peer(1), 42, true).is_err());
        let retained = controller.records.values().next().unwrap();
        assert_eq!(retained.request.width, width);
        assert_eq!(retained.request.options, options);
    }

    #[test]
    fn policy_rejection_after_candidate_projection_leaves_owner_unchanged() {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        controller.set_global_display_policy(7, json!({}));
        assert!(controller.launch(1, request(1), peer(1), now).is_none());
        let retained = controller.records.values_mut().next().unwrap();
        retained.application_display_overrides = Some(json!({}));
        retained.application_launch = Some(77);
        let mut invalid = request(2);
        invalid.audio_channels = 6;
        let retained = controller.records.values().next().unwrap();
        assert!(ResumeSnapshot::build(&controller, retained, invalid, peer(1), 42, true).is_err());
        let retained = controller.records.values().next().unwrap();
        assert_eq!(retained.application_launch, Some(77));
        assert_eq!(retained.request.audio_channels, 2);
    }
}
