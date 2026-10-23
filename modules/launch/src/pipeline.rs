use crate::{
    SERVICE,
    protocol::*,
    records::{
        CatalogSelection, DisplayAdmission, GlobalPreparation, Records,
        random_display_admission_token,
    },
    responses,
    workflow::{Step, Workflow, first_cleanup_step},
};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

type Transport = ModuleResult<Response>;

fn display_admission_eligible(record: &crate::records::Record, now: Instant) -> bool {
    record.catalog_selection == CatalogSelection::Exported
        && record.preflight_ready
        && record.preflight_token.is_some()
        && if record.launch_started {
            record.state == State::Active
        } else {
            record.state == State::Starting
                && record
                    .preflight_deadline
                    .is_some_and(|deadline| now < deadline)
        }
}

fn display_policy_snapshot(
    global_revision: Option<u64>,
    global_overrides: &BTreeMap<String, Value>,
    record: &crate::records::Record,
    certificate_id: &str,
    client_overrides: &BTreeMap<String, Value>,
) -> Value {
    let mut effective = global_overrides.clone();
    effective.extend(record.application_display_overrides.clone());
    effective.extend(client_overrides.clone());
    serde_json::json!({
        "global_revision":global_revision,
        "global_overrides":global_overrides,
        "catalog_revision":record.catalog_revision,
        "certificate_id":certificate_id,
        "application_overrides":record.application_display_overrides,
        "client_overrides":client_overrides,
        "effective_overrides":effective,
    })
}

fn valid_frame_limiter_snapshot(snapshot: &BTreeMap<String, Value>) -> bool {
    let Some(settings) = snapshot.get("settings").and_then(Value::as_object) else {
        return false;
    };
    if snapshot.get("version").and_then(Value::as_u64) != Some(1)
        || snapshot
            .get("requested_stream_fps_millihz")
            .and_then(Value::as_u64)
            .is_none_or(|rate| rate == 0 || rate > 1_000_000)
        || snapshot.len() != 9
        || settings.len() != 11
    {
        return false;
    }
    for key in [
        "client_display_mode_override_millihz",
        "lossless_scaling_limit_millihz",
    ] {
        if !snapshot.get(key).is_some_and(|value| {
            value.is_null() || value.as_u64().is_some_and(|rate| rate <= 1_000_000)
        }) {
            return false;
        }
    }
    for key in ["virtual_display_active", "physical_framegen_capture"] {
        if !snapshot
            .get(key)
            .is_some_and(|value| value.is_null() || value.is_boolean())
        {
            return false;
        }
    }
    if !snapshot
        .get("game_frame_generation_mode")
        .is_some_and(|value| {
            value.is_null()
                || value.as_str().is_some_and(|mode| {
                    matches!(
                        mode,
                        "none"
                            | "lossless-scaling"
                            | "game-provided"
                            | "nvidia-smooth-motion"
                            | "other"
                    )
                })
        })
        || !snapshot
            .get("rtss_frame_limit_type_explicit_override")
            .is_some_and(|value| value.is_null() || value.is_boolean())
    {
        return false;
    }
    for key in [
        "frame_limiter_enable",
        "frame_limiter_disable_vsync",
        "rtss_allow_virtual_display_override",
        "mangohud_always_show_graph",
    ] {
        if !settings.get(key).is_some_and(Value::is_boolean) {
            return false;
        }
    }
    if settings
        .get("frame_limiter_fps_limit_millihz")
        .and_then(Value::as_u64)
        .is_none_or(|rate| rate > 1_000_000)
    {
        return false;
    }
    let string = |key| settings.get(key).and_then(Value::as_str);
    string("frame_limiter_provider").is_some_and(valid_host_frame_limiter_provider)
        && string("frame_limiter_auto_virtual_framegen")
            .is_some_and(|mode| matches!(mode, "enabled" | "vrr" | "legacy" | "disabled"))
        && string("rtss_frame_limit_type").is_some_and(|mode| {
            matches!(
                mode,
                "async" | "front edge sync" | "back edge sync" | "nvidia reflex"
            )
        })
        && string("rtss_install_path").is_some_and(valid_rtss_install_path)
        && string("mangohud_limiter_method")
            .is_some_and(|method| matches!(method, "early" | "late"))
        && string("mangohud_preset")
            .is_some_and(|preset| matches!(preset, "custom" | "1" | "2" | "3" | "4"))
}

fn valid_client_frame_limiter_overrides(overrides: &BTreeMap<String, Value>) -> bool {
    overrides.iter().all(|(key, value)| {
        if !matches!(
            key.as_str(),
            "frame_limiter_enable"
                | "frame_limiter_disable_vsync"
                | "rtss_allow_virtual_display_override"
                | "mangohud_always_show_graph"
                | "frame_limiter_fps_limit"
                | "frame_limiter_provider"
                | "frame_limiter_auto_virtual_framegen"
                | "rtss_frame_limit_type"
                | "rtss_install_path"
                | "mangohud_limiter_method"
                | "mangohud_preset"
        ) {
            return false;
        }
        if value.is_null() {
            return true;
        }
        match key.as_str() {
            "frame_limiter_enable"
            | "frame_limiter_disable_vsync"
            | "rtss_allow_virtual_display_override"
            | "mangohud_always_show_graph" => value.is_boolean(),
            "frame_limiter_fps_limit" => value.as_f64().is_some_and(|fps| {
                fps.is_finite()
                    && (0.0..=1000.0).contains(&fps)
                    && ((fps * 1000.0) - (fps * 1000.0).round()).abs() < 0.000_001
            }),
            "frame_limiter_provider" => value
                .as_str()
                .is_some_and(valid_host_frame_limiter_provider),
            "frame_limiter_auto_virtual_framegen" => value
                .as_str()
                .is_some_and(|mode| matches!(mode, "enabled" | "vrr" | "legacy" | "disabled")),
            "rtss_frame_limit_type" => {
                cfg!(windows)
                    && value.as_str().is_some_and(|mode| {
                        matches!(
                            mode,
                            "async" | "front edge sync" | "back edge sync" | "nvidia reflex"
                        )
                    })
            }
            "rtss_install_path" => value.as_str().is_some_and(valid_rtss_install_path),
            "mangohud_limiter_method" => value
                .as_str()
                .is_some_and(|method| matches!(method, "early" | "late")),
            "mangohud_preset" => value
                .as_str()
                .is_some_and(|preset| matches!(preset, "custom" | "1" | "2" | "3" | "4")),
            _ => false,
        }
    })
}

fn valid_host_frame_limiter_provider(provider: &str) -> bool {
    #[cfg(windows)]
    {
        matches!(provider, "auto" | "none" | "rtss" | "nvidia-control-panel")
    }
    #[cfg(target_os = "linux")]
    {
        matches!(
            provider,
            "auto" | "none" | "mangohud" | "proton" | "mangohud-proton"
        )
    }
    #[cfg(target_os = "macos")]
    {
        matches!(provider, "auto" | "none")
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = provider;
        false
    }
}

fn valid_rtss_install_path(path: &str) -> bool {
    if path.len() > 4096 || path.chars().any(char::is_control) {
        return false;
    }
    #[cfg(windows)]
    {
        if path.is_empty() {
            return true;
        }
        let bytes = path.as_bytes();
        let drive_prefix = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
        let drive_root = drive_prefix && bytes.len() >= 3 && matches!(bytes[2], b'\\' | b'/');
        let unc_root = path.starts_with("\\\\") || path.starts_with("//");
        let valid_unc = unc_root
            && path[2..]
                .split(['\\', '/'])
                .filter(|part| !part.is_empty())
                .count()
                >= 2;
        let relative = !matches!(bytes.first().copied(), Some(b'\\' | b'/')) && !drive_prefix;
        drive_root || valid_unc || relative
    }
    #[cfg(not(windows))]
    {
        path.is_empty()
    }
}

fn snapshot_matches_application_policy(
    record: &crate::records::Record,
    snapshot: &BTreeMap<String, Value>,
    client: Option<&BTreeMap<String, Value>>,
) -> bool {
    let Some(settings) = snapshot.get("settings").and_then(Value::as_object) else {
        return false;
    };
    let application = record
        .config_overrides
        .as_ref()
        .map(|overrides| serde_json::to_value(overrides).expect("serializable overrides"))
        .unwrap_or_else(|| serde_json::json!({}));
    let explicit_rtss_type = client
        .and_then(|overrides| overrides.get("rtss_frame_limit_type"))
        .filter(|value| !value.is_null())
        .or_else(|| application.get("rtss_frame_limit_type"))
        .is_some();
    let provenance = snapshot.get("rtss_frame_limit_type_explicit_override");
    if explicit_rtss_type && provenance != Some(&Value::Bool(true)) {
        return false;
    }
    if !explicit_rtss_type && provenance == Some(&Value::Bool(true)) {
        return false;
    }
    for (key, effective_key) in [
        ("frame_limiter_enable", "frame_limiter_enable"),
        ("frame_limiter_provider", "frame_limiter_provider"),
        ("frame_limiter_disable_vsync", "frame_limiter_disable_vsync"),
        (
            "frame_limiter_auto_virtual_framegen",
            "frame_limiter_auto_virtual_framegen",
        ),
        ("rtss_frame_limit_type", "rtss_frame_limit_type"),
        (
            "rtss_allow_virtual_display_override",
            "rtss_allow_virtual_display_override",
        ),
        ("rtss_install_path", "rtss_install_path"),
        ("mangohud_limiter_method", "mangohud_limiter_method"),
        ("mangohud_preset", "mangohud_preset"),
        ("mangohud_always_show_graph", "mangohud_always_show_graph"),
    ] {
        let expected = client
            .and_then(|overrides| overrides.get(key))
            .filter(|value| !value.is_null())
            .or_else(|| application.get(key));
        if let Some(expected) = expected
            && settings.get(effective_key) != Some(expected)
        {
            return false;
        }
    }
    let fps = client
        .and_then(|overrides| overrides.get("frame_limiter_fps_limit"))
        .filter(|value| !value.is_null())
        .or_else(|| application.get("frame_limiter_fps_limit"));
    if let Some(fps) = fps {
        let Some(hertz) = fps.as_f64() else {
            return false;
        };
        if !hertz.is_finite()
            || !(0.0..=1000.0).contains(&hertz)
            || settings
                .get("frame_limiter_fps_limit_millihz")
                .and_then(Value::as_u64)
                != Some((hertz * 1000.0).round() as u64)
        {
            return false;
        }
    }
    true
}
pub struct Pipeline {
    records: Records,
    job: Option<Workflow>,
    sequence: u64,
    monitor_after: u64,
    ingress_since_monitor: u8,
    environment: BTreeMap<String, String>,
}
impl Pipeline {
    pub fn new(environment: BTreeMap<String, String>) -> Result<Self, &'static str> {
        Ok(Self {
            records: Records::new()?,
            job: None,
            sequence: 0,
            monitor_after: 0,
            ingress_since_monitor: 0,
            environment,
        })
    }
    pub fn tick(&mut self, mut send: impl FnMut(Command) -> Transport) -> ModuleResult<()> {
        if let Some(mut job) = self.job.take() {
            if let Some(request) = job.pending.take() {
                match send(Command::TakeResult { request }) {
                    Ok(Response::Pending) => {
                        job.pending = Some(request);
                        self.job = Some(job);
                        return Ok(());
                    }
                    Ok(Response::Result { payload }) => {
                        self.consume(&mut job, payload, &mut send)?
                    }
                    _ => {
                        let record = self
                            .records
                            .values
                            .get_mut(&job.launch_id)
                            .expect("workflow record");
                        record.outstanding_request = Some(request);
                        if !job.cleanup {
                            record.cleanup_outcome = Outcome::Failed;
                        }
                        if self.settle_outstanding(job.launch_id, &mut send) {
                            self.dependency_failed(&mut job, "dependency_unavailable", &mut send)?;
                        } else {
                            self.cleanup_failed(
                                &job,
                                "request_cancellation_unconfirmed",
                                &mut send,
                            )?;
                        }
                    }
                }
            } else {
                self.dispatch(&mut job, &mut send)?;
            }
            return Ok(());
        }
        // Background exit cleanup gets a turn even under continuous ingress.
        if self.ingress_since_monitor >= 8 {
            self.ingress_since_monitor = 0;
            self.schedule_monitor();
            if self.job.is_some() {
                return Ok(());
            }
        }
        match send(Command::Poll {})? {
            Response::Request {
                id,
                caller,
                initiator,
                service,
                payload,
                remaining_ms,
            } => {
                self.ingress_since_monitor = self.ingress_since_monitor.saturating_add(1);
                if service != SERVICE {
                    return complete(&mut send, id, error("invalid_service"));
                }
                let operation = serde_json::from_value::<Operation>(payload)
                    .map_err(|_| "invalid_request")
                    .and_then(|operation| {
                        operation.validate()?;
                        Ok(operation)
                    });
                match operation {
                    Ok(operation) => {
                        self.receive(id, &caller, &initiator, remaining_ms, operation, &mut send)
                    }
                    Err(code) => complete(&mut send, id, error(code)),
                }
            }
            Response::Pending => {
                self.schedule_monitor();
                Ok(())
            }
            _ => Err("launch poll failed".into()),
        }
    }
    fn receive(
        &mut self,
        request: u64,
        caller: &str,
        initiator: &str,
        remaining_ms: u64,
        operation: Operation,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        match operation {
            Operation::PreflightExport {
                operation_id,
                app_id,
                global_preparation,
                legacy_global_preparation,
            } => match self.records.start(
                caller,
                initiator,
                operation_id,
                app_id,
                CatalogSelection::Exported,
                GlobalPreparation {
                    structured_steps: global_preparation,
                    legacy_commands: legacy_global_preparation,
                },
            ) {
                Ok((id, true)) => {
                    let record = self.records.values.get_mut(&id).expect("preflight record");
                    record.preflight_token = Some(record.session_key.clone());
                    self.job = Some(Workflow::launch(id, request, remaining_ms));
                    Ok(())
                }
                Ok((id, false)) if self.records.values[&id].preflight_ready => {
                    complete(send, request, self.records.values[&id].preflight_response())
                }
                Ok(_) => complete(send, request, error("preflight_unavailable")),
                Err(code) => complete(send, request, error(code)),
            },
            Operation::Launch {
                operation_id,
                app_id,
                global_preparation,
                legacy_global_preparation,
            } => match self.records.start(
                caller,
                initiator,
                operation_id,
                app_id,
                CatalogSelection::Initiator,
                GlobalPreparation {
                    structured_steps: global_preparation,
                    legacy_commands: legacy_global_preparation,
                },
            ) {
                Ok((id, true)) => {
                    self.job = Some(Workflow::launch(id, request, remaining_ms));
                    Ok(())
                }
                Ok((id, false)) => complete(send, request, self.records.values[&id].response()),
                Err(code) => complete(send, request, error(code)),
            },
            Operation::LaunchExport {
                operation_id,
                app_id,
                global_preparation,
                legacy_global_preparation,
                preflight_token,
                display_admission_token,
                frame_limiter_settings,
                client_frame_limiter_overrides,
                application_display_overrides,
            } => {
                if application_display_overrides.is_some() && preflight_token.is_none() {
                    return complete(send, request, error("preflight_required"));
                }
                if application_display_overrides
                    .as_ref()
                    .is_some_and(|overrides| !valid_application_display_overrides(overrides))
                {
                    return complete(
                        send,
                        request,
                        error("invalid_application_display_overrides"),
                    );
                }
                if frame_limiter_settings.is_some() && preflight_token.is_none() {
                    return complete(send, request, error("preflight_required"));
                }
                if client_frame_limiter_overrides.is_some() && preflight_token.is_none() {
                    return complete(send, request, error("preflight_required"));
                }
                if preflight_token.is_some() && frame_limiter_settings.is_none() {
                    return complete(send, request, error("invalid_frame_limiter_snapshot"));
                }
                if let Some(settings) = &frame_limiter_settings
                    && !valid_frame_limiter_snapshot(settings)
                {
                    return complete(send, request, error("invalid_frame_limiter_snapshot"));
                }
                if client_frame_limiter_overrides
                    .as_ref()
                    .is_some_and(|overrides| !valid_client_frame_limiter_overrides(overrides))
                {
                    return complete(
                        send,
                        request,
                        error("invalid_client_frame_limiter_overrides"),
                    );
                }
                match self.records.start(
                    caller,
                    initiator,
                    operation_id,
                    app_id,
                    CatalogSelection::Exported,
                    GlobalPreparation {
                        structured_steps: global_preparation,
                        legacy_commands: legacy_global_preparation,
                    },
                ) {
                    Ok((id, true)) => {
                        if preflight_token.is_some() || display_admission_token.is_some() {
                            self.records.values.remove(&id);
                            return complete(send, request, error("preflight_not_found"));
                        }
                        self.records
                            .values
                            .get_mut(&id)
                            .expect("launch record")
                            .launch_started = true;
                        self.job = Some(Workflow::launch(id, request, remaining_ms));
                        Ok(())
                    }
                    Ok((id, false)) => {
                        let record = self.records.values.get_mut(&id).expect("launch record");
                        if record.preflight_token.as_deref() != preflight_token.as_deref()
                            || (record.preflight_token.is_some() && !record.preflight_ready)
                        {
                            return complete(send, request, error("preflight_mismatch"));
                        }
                        if record.application_display_overrides
                            != application_display_overrides.clone().unwrap_or_default()
                        {
                            return complete(
                                send,
                                request,
                                error("application_display_policy_mismatch"),
                            );
                        }
                        match (
                            &record.display_admission,
                            display_admission_token.as_deref(),
                        ) {
                            (Some(admission), Some(token)) if admission.token == token => {}
                            (Some(admission), None) if !admission.launch_token_required => {}
                            (None, None) => {}
                            _ => return complete(send, request, error("display_admission_denied")),
                        }
                        if let Some(settings) = &frame_limiter_settings
                            && !snapshot_matches_application_policy(
                                record,
                                settings,
                                client_frame_limiter_overrides.as_ref(),
                            )
                        {
                            return complete(send, request, error("frame_limiter_policy_mismatch"));
                        }
                        if record.launch_started {
                            if record.frame_limiter_settings != frame_limiter_settings
                                || record.client_frame_limiter_overrides
                                    != client_frame_limiter_overrides
                            {
                                return complete(send, request, error("operation_conflict"));
                            }
                            return complete(send, request, record.response());
                        }
                        record.frame_limiter_settings = frame_limiter_settings;
                        record.client_frame_limiter_overrides = client_frame_limiter_overrides;
                        record.launch_started = true;
                        let mut workflow = Workflow::launch(id, request, remaining_ms);
                        workflow.step = if record.lutris_source_id.is_some() {
                            Step::LutrisStatus
                        } else {
                            Step::CreateSession
                        };
                        self.job = Some(workflow);
                        Ok(())
                    }
                    Err(code) => complete(send, request, error(code)),
                }
            }
            Operation::PrepareDisplayAdmission {
                application_operation_id,
                global_revision,
                global_overrides,
                catalog_revision,
                application_display_overrides,
                certificate_id,
                client_display_overrides,
                rtsp_operation_id,
                request_sha256,
            } => {
                if caller != "amaterasu.nvhttp"
                    || (global_revision.is_none() && !global_overrides.is_empty())
                {
                    return complete(send, request, error("display_admission_denied"));
                }
                let Some(record) = self.records.values.values_mut().find(|record| {
                    record.owner == caller && record.operation_id == application_operation_id
                }) else {
                    return complete(send, request, error("display_admission_denied"));
                };
                if !display_admission_eligible(record, Instant::now())
                    || record.preflight_token.is_none()
                    || record.catalog_revision != Some(catalog_revision)
                    || record.application_display_overrides != application_display_overrides
                {
                    return complete(send, request, error("display_admission_denied"));
                }
                let snapshot = display_policy_snapshot(
                    global_revision,
                    &global_overrides,
                    record,
                    &certificate_id,
                    &client_display_overrides,
                );
                if !serde_json::to_vec(&snapshot).is_ok_and(|bytes| bytes.len() <= 32_768) {
                    return complete(send, request, error("display_admission_denied"));
                }
                if let Some(admission) = &record.display_admission {
                    if admission.certificate_id != certificate_id
                        || admission.rtsp_operation_id != rtsp_operation_id
                        || admission.request_sha256 != request_sha256
                        || admission.snapshot != snapshot
                    {
                        return complete(send, request, error("display_admission_denied"));
                    }
                    return complete(
                        send,
                        request,
                        serde_json::json!({
                            "ok":true,
                            "display_admission_token":admission.token,
                            "display_policy_snapshot":admission.snapshot,
                            "application_operation_id":application_operation_id,
                            "rtsp_operation_id":rtsp_operation_id,
                            "request_sha256":request_sha256,
                        }),
                    );
                }
                let token = match random_display_admission_token() {
                    Ok(token) => token,
                    Err(code) => return complete(send, request, error(code)),
                };
                record.display_admission = Some(DisplayAdmission {
                    token: token.clone(),
                    launch_token_required: !record.launch_started,
                    certificate_id,
                    rtsp_operation_id: rtsp_operation_id.clone(),
                    request_sha256: request_sha256.clone(),
                    snapshot: snapshot.clone(),
                });
                complete(
                    send,
                    request,
                    serde_json::json!({
                        "ok":true,
                        "display_admission_token":token,
                        "display_policy_snapshot":snapshot,
                        "application_operation_id":application_operation_id,
                        "rtsp_operation_id":rtsp_operation_id,
                        "request_sha256":request_sha256,
                    }),
                )
            }
            Operation::VerifyDisplayAdmission {
                token,
                application_operation_id,
                rtsp_operation_id,
                request_sha256,
                display_policy_snapshot,
                certificate_id,
                catalog_revision,
            } => {
                if caller != "amaterasu.rtsp" {
                    return complete(send, request, error("display_admission_denied"));
                }
                let now = Instant::now();
                let remaining_ms = self.records.values.values().find_map(|record| {
                    let verified = record.owner == "amaterasu.nvhttp"
                        && record.operation_id == application_operation_id
                        && display_admission_eligible(record, now)
                        && record.catalog_revision == Some(catalog_revision)
                        && record.display_admission.as_ref().is_some_and(|admission| {
                            admission.token == token
                                && admission.certificate_id == certificate_id
                                && admission.rtsp_operation_id == rtsp_operation_id
                                && admission.request_sha256 == request_sha256
                                && admission.snapshot == display_policy_snapshot
                        });
                    if !verified {
                        return None;
                    }
                    let remaining = if record.launch_started {
                        None
                    } else {
                        let deadline = record.preflight_deadline?;
                        Some(
                            u64::try_from(deadline.duration_since(now).as_millis())
                                .unwrap_or(120_000)
                                .clamp(1, 120_000),
                        )
                    };
                    Some(remaining)
                });
                let Some(remaining_ms) = remaining_ms else {
                    return complete(send, request, error("display_admission_denied"));
                };
                let mut response = serde_json::json!({
                    "ok":true,
                    "verified":true,
                    "rtsp_operation_id":rtsp_operation_id,
                    "request_sha256":request_sha256,
                });
                if let Some(remaining_ms) = remaining_ms {
                    response["preflight_remaining_ms"] = serde_json::json!(remaining_ms);
                }
                complete(send, request, response)
            }
            Operation::VerifyFrameLimiterSnapshot {
                application_operation_id,
                app_id,
                catalog_revision,
                frame_limiter_settings,
                frame_limiter_executable_path,
            } => {
                let verified = caller == "amaterasu.rtsp"
                    && self.records.values.values().any(|record| {
                        record.owner == "amaterasu.nvhttp"
                            && record.catalog_selection == CatalogSelection::Exported
                            && record.preflight_ready
                            && record.preflight_token.is_some()
                            && record.launch_started
                            && record.state == State::Active
                            && record.operation_id == application_operation_id
                            && record.app_id == app_id
                            && record.catalog_revision == Some(catalog_revision)
                            && record.frame_limiter_settings.as_ref()
                                == Some(&frame_limiter_settings)
                            && record.executable_path.as_deref()
                                == Some(frame_limiter_executable_path.as_str())
                    });
                if !verified {
                    return complete(send, request, error("frame_limiter_snapshot_denied"));
                }
                complete(
                    send,
                    request,
                    serde_json::json!({
                        "ok":true,
                        "verified":true,
                        "application_operation_id":application_operation_id,
                        "app_id":app_id,
                        "catalog_revision":catalog_revision,
                        "frame_limiter_settings":frame_limiter_settings,
                        "frame_limiter_executable_path":frame_limiter_executable_path,
                    }),
                )
            }
            Operation::Lookup { operation_id } => {
                let response = self
                    .records
                    .owned_operation(caller, &operation_id)
                    .map(|record| record.response())
                    .unwrap_or_else(error);
                complete(send, request, response)
            }
            Operation::Stop { launch_id } => {
                let record = match self.records.owned(caller, launch_id) {
                    Ok(record) => record,
                    Err(code) => return complete(send, request, error(code)),
                };
                if record.state.terminal() {
                    return complete(send, request, record.response());
                }
                let outcome = if record.state == State::CleanupFailed {
                    record.cleanup_outcome
                } else {
                    Outcome::Completed
                };
                self.records
                    .values
                    .get_mut(&launch_id)
                    .expect("owned launch record")
                    .display_admission = None;
                let mut job = Workflow::launch(launch_id, request, remaining_ms);
                job.begin_cleanup(
                    self.records
                        .values
                        .get_mut(&launch_id)
                        .expect("owned record"),
                    outcome,
                    None,
                );
                self.job = Some(job);
                Ok(())
            }
            Operation::Status { launch_id } => {
                let response = self
                    .records
                    .owned(caller, launch_id)
                    .map(|record| record.response())
                    .unwrap_or_else(error);
                complete(send, request, response)
            }
            Operation::List { offset, limit } => {
                complete(send, request, self.records.list(caller, offset, limit))
            }
        }
    }
    /// Cancel prevents a queued request from reaching its provider; release removes
    /// its retained broker entry. Already delivered mutations still need lookup.
    /// Keep the ID on failure so explicit stop can retry without freeing a lease.
    fn settle_outstanding(
        &mut self,
        launch_id: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> bool {
        let record = self
            .records
            .values
            .get_mut(&launch_id)
            .expect("workflow record");
        let Some(request) = record.outstanding_request else {
            return true;
        };
        for command in [
            Command::Cancel { request },
            Command::ReleaseResult { request },
        ] {
            match send(command) {
                Ok(Response::Ok) => {}
                // An ephemeral TakeResult may have released the entry before
                // its transport failed; broker expiration also removes entries.
                Ok(Response::Error { code, .. }) if code == "not_found" => {}
                _ => return false,
            }
        }
        record.outstanding_request = None;
        true
    }

    fn schedule_monitor(&mut self) {
        let next = self
            .records
            .values
            .values()
            .filter(|record| is_monitored(record))
            .find(|record| record.launch_id > self.monitor_after)
            .or_else(|| {
                self.records
                    .values
                    .values()
                    .find(|record| is_monitored(record))
            });
        if let Some(record) = next {
            self.ingress_since_monitor = 0;
            self.monitor_after = record.launch_id;
            let playnite = matches!(
                record.target,
                Some(LaunchTarget::Playnite { .. } | LaunchTarget::PlayniteFullscreen { .. })
            );
            self.job = Some(Workflow::monitor(record.launch_id, playnite));
        }
    }
    fn dispatch(
        &mut self,
        job: &mut Workflow,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if !self.settle_outstanding(job.launch_id, send) {
            return self.cleanup_failed(job, "request_cancellation_unconfirmed", send);
        }
        if job.cleanup
            && job.step == Step::Finish
            && self.records.values[&job.launch_id].session_id.is_none()
        {
            return self.finish(job, send);
        }
        if Instant::now() >= job.deadline {
            return self.dependency_failed(job, "deadline_exceeded", send);
        }
        if !job.cleanup
            && let Some(request) = job.request
            && !matches!(send(Command::CheckRequest { request }), Ok(Response::Ok))
        {
            job.begin_cleanup(
                self.records
                    .values
                    .get_mut(&job.launch_id)
                    .expect("workflow record"),
                Outcome::Cancelled,
                Some("request_cancelled"),
            );
            return self.retain(job);
        }
        let record = self
            .records
            .values
            .get_mut(&job.launch_id)
            .expect("workflow record");
        let (service, payload) = job
            .command(record, &self.environment)
            .map_err(str::to_owned)?;
        // Mark possibly committed mutations before submission. A transport or
        // cancellation error then reconciles by owned lookup, never by respawn.
        if job.step == Step::CreateSession {
            record.session_requested = true;
        }
        if job.step == Step::Spawn {
            record.process_requested = true;
        }
        if job.step == Step::PrepareApplication {
            record.preparation_requested = true;
        }
        if job.step == Step::StartPlaynite {
            record.provider_launch_requested = true;
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("launch sequence exhausted")?;
        let timeout_ms = job
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .clamp(1, 5_000) as u64;
        let response = send(Command::Request {
            service: service.into(),
            payload,
            timeout_ms,
            operation_id: format!("{}.{}", record.session_key, self.sequence),
            parent: job.parent(),
        });
        match response {
            Ok(Response::Accepted { id }) => {
                job.pending = Some(id);
                self.retain(job)
            }
            _ => self.dependency_failed(job, "dependency_unavailable", send),
        }
    }
    fn consume(
        &mut self,
        job: &mut Workflow,
        payload: Value,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if job.cleanup {
            return self.consume_cleanup(job, payload, send);
        }
        if payload["ok"] != true {
            if matches!(job.step, Step::LutrisStatus | Step::LutrisPreSpawn) {
                return self.dependency_failed(job, "lutris_unavailable", send);
            }
            if job.step == Step::StartPlaynite && responses::provider_start_not_started(&payload) {
                let record = self
                    .records
                    .values
                    .get_mut(&job.launch_id)
                    .expect("workflow record");
                record.provider_launch_requested = false;
                record.provider_stopped = true;
                job.begin_cleanup(record, Outcome::Failed, Some("playnite_launch_rejected"));
                return self.retain(job);
            }
            return self.dependency_failed(job, "dependency_rejected", send);
        }
        let record = self
            .records
            .values
            .get_mut(&job.launch_id)
            .expect("workflow record");
        match job.step {
            Step::Catalog => {
                if payload["id"] != record.app_id {
                    return self.dependency_failed(job, "invalid_catalog_response", send);
                }
                match record.catalog_selection {
                    CatalogSelection::Initiator if payload.get("catalog_owner").is_some() => {
                        return self.dependency_failed(job, "invalid_catalog_response", send);
                    }
                    CatalogSelection::Exported => {
                        let Some(owner) = payload["catalog_owner"]
                            .as_str()
                            .filter(|owner| valid_catalog_principal(owner))
                        else {
                            return self.dependency_failed(job, "invalid_catalog_response", send);
                        };
                        if record.set_exported_catalog_owner(owner).is_err() {
                            return self.dependency_failed(job, "invalid_catalog_response", send);
                        }
                    }
                    CatalogSelection::Initiator => {}
                }
                match LaunchTarget::from_catalog_entry_with_preparations(
                    &payload["entry"],
                    &record.global_preparation,
                    &record.legacy_global_preparation,
                    &self.environment,
                ) {
                    Ok(target) => {
                        record.executable_path = match &target {
                            LaunchTarget::Executable(executable) => {
                                Some(executable.program.clone())
                            }
                            _ => None,
                        };
                        record.target = Some(target);
                    }
                    Err(code) => return self.dependency_failed(job, code, send),
                }
                record.lutris_source_id = match payload.get("provider_provenance") {
                    None => None,
                    Some(provenance) => {
                        let valid_object = provenance.as_object().is_some_and(|fields| {
                            fields.len() == 2 && provenance["provider"] == "amaterasu.lutris"
                        });
                        let source_id = provenance["source_id"].as_str().and_then(|source| {
                            source
                                .parse::<i64>()
                                .ok()
                                .filter(|id| *id > 0 && id.to_string() == source)
                        });
                        if !valid_object
                            || source_id.is_none()
                            || !matches!(record.target, Some(LaunchTarget::Executable(_)))
                        {
                            return self.dependency_failed(job, "invalid_catalog_response", send);
                        }
                        source_id
                    }
                };
                let config_overrides = match payload["entry"].get("config_overrides") {
                    Some(value) => match serde_json::from_value::<
                        crate::protocol::ApplicationConfigOverrides,
                    >(value.clone())
                    {
                        Ok(overrides) if overrides.validate().is_ok() => overrides,
                        _ => return self.dependency_failed(job, "invalid_catalog_response", send),
                    },
                    None => crate::protocol::ApplicationConfigOverrides::default(),
                };
                record.config_overrides =
                    (!config_overrides.is_empty()).then_some(config_overrides);
                record.application_display_overrides = record
                    .config_overrides
                    .as_ref()
                    .map(|overrides| overrides.display_overrides())
                    .unwrap_or_default();
                let Some(revision) = payload.get("revision").and_then(Value::as_u64) else {
                    return self.dependency_failed(job, "invalid_catalog_response", send);
                };
                record.catalog_revision = Some(revision);
                if record.preflight_token.is_some() {
                    // The caller can only use the token after this response. A slow Catalog
                    // resolution must not spend the token's lifetime before it is issued.
                    record.preflight_deadline = Some(Instant::now() + Duration::from_secs(120));
                    record.preflight_ready = true;
                    let response = record.preflight_response();
                    return complete(send, job.request.expect("preflight request"), response);
                }
                job.step = if record.lutris_source_id.is_some() {
                    Step::LutrisStatus
                } else {
                    Step::CreateSession
                };
            }
            Step::LutrisStatus => {
                if payload["settings_ready"] != true
                    || payload["enabled"] != true
                    || payload["available"] != true
                {
                    return self.dependency_failed(job, "lutris_unavailable", send);
                }
                job.step = Step::CreateSession;
            }
            Step::LutrisPreSpawn => {
                if payload["settings_ready"] != true
                    || payload["enabled"] != true
                    || payload["available"] != true
                {
                    return self.dependency_failed(job, "lutris_unavailable", send);
                }
                job.step = Step::Spawn;
            }
            Step::CreateSession => {
                let session = match responses::session(&payload, record) {
                    Ok(session) if session["state"] == "preparing" => session,
                    _ => return self.dependency_failed(job, "invalid_session_response", send),
                };
                record.session_id = session["id"].as_u64();
                job.step = Step::Acquire;
            }
            Step::Acquire => {
                let valid = responses::session(&payload, record).is_ok_and(|session| {
                    session["state"] == "preparing"
                        && record
                            .resource
                            .as_deref()
                            .is_some_and(|resource| session["leases"][resource] == "exclusive")
                });
                if !valid {
                    return self.dependency_failed(job, "invalid_session_response", send);
                }
                job.step = match record.target.as_ref() {
                    Some(LaunchTarget::Executable(_)) if record.lutris_source_id.is_some() => {
                        Step::LutrisPreSpawn
                    }
                    Some(LaunchTarget::Executable(_)) => Step::Spawn,
                    Some(LaunchTarget::Playnite { preparation, .. })
                    | Some(LaunchTarget::PlayniteFullscreen { preparation, .. })
                    | Some(LaunchTarget::Desktop { preparation, .. })
                        if !preparation.is_empty() =>
                    {
                        Step::PrepareApplication
                    }
                    Some(
                        LaunchTarget::Playnite { .. } | LaunchTarget::PlayniteFullscreen { .. },
                    ) => Step::StartPlaynite,
                    Some(LaunchTarget::Desktop { .. }) => Step::Activate,
                    None => return self.dependency_failed(job, "invalid_catalog_response", send),
                };
            }
            Step::PrepareApplication | Step::AwaitPreparation => {
                let (_, state) = match responses::preparation(&payload, record) {
                    Ok(preparation) => preparation,
                    Err(code) => return self.dependency_failed(job, code, send),
                };
                match state {
                    responses::PreparationState::Preparing
                    | responses::PreparationState::Cleaning => {
                        job.step = Step::AwaitPreparation;
                    }
                    responses::PreparationState::Ready => {
                        job.step = if matches!(record.target, Some(LaunchTarget::Desktop { .. })) {
                            Step::Activate
                        } else {
                            Step::StartPlaynite
                        };
                    }
                    responses::PreparationState::PreparationFailed => job.begin_cleanup(
                        record,
                        Outcome::Failed,
                        Some("global_preparation_failed"),
                    ),
                    responses::PreparationState::CleanupFailed => {
                        job.begin_cleanup(
                            record,
                            Outcome::Failed,
                            Some("global_preparation_failed"),
                        );
                        return self.cleanup_failed(job, "global_preparation_cleanup_failed", send);
                    }
                    responses::PreparationState::Complete => {
                        return self.dependency_failed(job, "invalid_preparation_response", send);
                    }
                }
            }
            Step::Spawn | Step::AwaitApplication => {
                let (process, state) = match responses::process(&payload, record) {
                    Ok(process) => process,
                    Err(code) => return self.dependency_failed(job, code, send),
                };
                if job.step == Step::Spawn {
                    record.process_id = process["process_id"].as_u64();
                }
                match state {
                    ApplicationState::Preparing | ApplicationState::Stopping => {
                        job.step = Step::AwaitApplication;
                    }
                    ApplicationState::Running => job.step = Step::Activate,
                    ApplicationState::Exited => job.begin_cleanup(record, Outcome::Completed, None),
                    ApplicationState::LaunchFailed => job.begin_cleanup(
                        record,
                        Outcome::Failed,
                        Some("application_launch_failed"),
                    ),
                    ApplicationState::Cancelled => {
                        job.begin_cleanup(record, Outcome::Cancelled, Some("application_cancelled"))
                    }
                    ApplicationState::CleanupFailed(outcome) => {
                        apply_application_outcome(record, outcome);
                        return self.cleanup_failed(job, "application_cleanup_failed", send);
                    }
                }
            }
            Step::StartPlaynite => {
                let provider = match responses::provider_launch(&payload, record) {
                    Ok(provider) => provider,
                    Err(code) => return self.dependency_failed(job, code, send),
                };
                record.provider_ticket = Some(provider.ticket);
                match provider.state {
                    responses::ProviderState::Starting => job.step = Step::AwaitPlayniteStart,
                    responses::ProviderState::Running => job.step = Step::Activate,
                    responses::ProviderState::Stopping => {
                        job.begin_cleanup(record, Outcome::Completed, None);
                    }
                    responses::ProviderState::Stopped => {
                        record.provider_stopped = true;
                        job.begin_cleanup(record, Outcome::Completed, None);
                    }
                    responses::ProviderState::LaunchFailed => {
                        record.provider_stopped = true;
                        job.begin_cleanup(record, Outcome::Failed, Some("playnite_launch_failed"));
                    }
                    responses::ProviderState::CleanupFailed => {
                        return self.cleanup_failed(job, "playnite_cleanup_unconfirmed", send);
                    }
                }
            }
            Step::AwaitPlayniteStart | Step::MonitorPlaynite => {
                let provider = match responses::provider_launch(&payload, record) {
                    Ok(provider) => provider,
                    Err(code) => return self.dependency_failed(job, code, send),
                };
                match provider.state {
                    responses::ProviderState::Starting if job.step == Step::AwaitPlayniteStart => {}
                    responses::ProviderState::Running if job.step == Step::AwaitPlayniteStart => {
                        job.step = Step::Activate
                    }
                    responses::ProviderState::Running if job.step == Step::MonitorPlaynite => {
                        return Ok(());
                    }
                    responses::ProviderState::Stopping => {
                        job.begin_cleanup(record, Outcome::Completed, None);
                    }
                    responses::ProviderState::Stopped => {
                        record.provider_stopped = true;
                        job.begin_cleanup(record, Outcome::Completed, None);
                    }
                    responses::ProviderState::LaunchFailed => {
                        record.provider_stopped = true;
                        job.begin_cleanup(record, Outcome::Failed, Some("playnite_launch_failed"));
                    }
                    responses::ProviderState::CleanupFailed => {
                        return self.cleanup_failed(job, "playnite_cleanup_unconfirmed", send);
                    }
                    _ => return self.dependency_failed(job, "invalid_provider_response", send),
                }
            }
            Step::Activate => {
                if !responses::session(&payload, record)
                    .is_ok_and(|session| session["state"] == "active")
                {
                    return self.dependency_failed(job, "invalid_session_response", send);
                }
                record.state = State::Active;
                return self.deliver_activation(job, send);
            }
            Step::Monitor => {
                let (_, state) = match responses::process(&payload, record) {
                    Ok(process) => process,
                    Err(code) => return self.dependency_failed(job, code, send),
                };
                match state {
                    ApplicationState::Running | ApplicationState::Stopping => return Ok(()),
                    ApplicationState::Exited => job.begin_cleanup(record, Outcome::Completed, None),
                    ApplicationState::LaunchFailed => job.begin_cleanup(
                        record,
                        Outcome::Failed,
                        Some("application_launch_failed"),
                    ),
                    ApplicationState::Cancelled => {
                        job.begin_cleanup(record, Outcome::Cancelled, Some("application_cancelled"))
                    }
                    ApplicationState::CleanupFailed(outcome) => {
                        apply_application_outcome(record, outcome);
                        return self.cleanup_failed(job, "application_cleanup_failed", send);
                    }
                    ApplicationState::Preparing => {
                        return self.dependency_failed(job, "invalid_application_response", send);
                    }
                }
            }
            _ => return Err("invalid launch workflow".into()),
        }
        self.retain(job)
    }
    fn consume_cleanup(
        &mut self,
        job: &mut Workflow,
        payload: Value,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let record = self
            .records
            .values
            .get_mut(&job.launch_id)
            .expect("workflow record");
        match job.step {
            Step::FindSession => {
                if payload["ok"] == true {
                    let session = match responses::session(&payload, record) {
                        Ok(session) => session,
                        Err(code) => return self.cleanup_failed(job, code, send),
                    };
                    record.session_id = session["id"].as_u64();
                } else if payload["code"] == "session_not_found" {
                    record.session_requested = false;
                } else {
                    return self.cleanup_failed(job, "session_lookup_failed", send);
                }
                job.step = first_cleanup_step(record);
            }
            Step::FindProcess => {
                if payload["ok"] == true {
                    let (process, _) = match responses::process(&payload, record) {
                        Ok(process) => process,
                        Err(code) => return self.cleanup_failed(job, code, send),
                    };
                    record.process_id = process["process_id"].as_u64();
                } else if payload["code"] == "process_not_found" {
                    record.process_requested = false;
                } else {
                    return self.cleanup_failed(job, "process_lookup_failed", send);
                }
                job.step = first_cleanup_step(record);
            }
            Step::FindPlaynite => {
                if responses::provider_lookup_not_found(&payload, record) {
                    job.step = Step::RecoverPlayniteStop;
                    return self.retain(job);
                }
                if payload["ok"] == true {
                    let provider = match responses::provider_launch(&payload, record) {
                        Ok(provider) => provider,
                        Err(code) => return self.cleanup_failed(job, code, send),
                    };
                    record.provider_ticket = Some(provider.ticket);
                    job.step = match provider.state {
                        responses::ProviderState::Stopped
                        | responses::ProviderState::LaunchFailed => {
                            record.provider_stopped = true;
                            first_cleanup_step(record)
                        }
                        responses::ProviderState::CleanupFailed => Step::RecoverPlayniteStop,
                        responses::ProviderState::Starting | responses::ProviderState::Running => {
                            Step::StopPlaynite
                        }
                        responses::ProviderState::Stopping => Step::AwaitPlayniteStop,
                    };
                } else if payload["code"] == "launch_not_found" {
                    job.step = Step::RecoverPlayniteStop;
                } else {
                    return self.cleanup_failed(job, "playnite_lookup_failed", send);
                }
            }
            Step::StopPreparation | Step::AwaitPreparationStop => {
                if payload["ok"] == false && payload["code"] == "preparation_not_found" {
                    record.preparation_stopped = true;
                    job.step = first_cleanup_step(record);
                    return self.retain(job);
                }
                let (_, state) = match responses::preparation(&payload, record) {
                    Ok(preparation) => preparation,
                    Err(code) => return self.cleanup_failed(job, code, send),
                };
                match state {
                    responses::PreparationState::Cleaning => {
                        job.step = Step::AwaitPreparationStop;
                    }
                    responses::PreparationState::Complete
                    | responses::PreparationState::PreparationFailed => {
                        record.preparation_stopped = true;
                        job.step = first_cleanup_step(record);
                    }
                    responses::PreparationState::CleanupFailed => {
                        return self.cleanup_failed(job, "global_preparation_cleanup_failed", send);
                    }
                    responses::PreparationState::Preparing | responses::PreparationState::Ready => {
                        return self.cleanup_failed(
                            job,
                            "global_preparation_stop_unconfirmed",
                            send,
                        );
                    }
                }
            }
            Step::RecoverPlayniteStop => {
                if payload["ok"] == false && payload["code"] == "launch_not_found" {
                    return self.cleanup_failed(job, "playnite_stop_unconfirmed", send);
                }
                record.provider_ticket = None;
                let provider = match responses::provider_launch(&payload, record) {
                    Ok(provider) => provider,
                    Err(code) => return self.cleanup_failed(job, code, send),
                };
                record.provider_ticket = Some(provider.ticket);
                match provider.state {
                    responses::ProviderState::Stopped | responses::ProviderState::LaunchFailed => {
                        record.provider_stopped = true;
                        job.step = first_cleanup_step(record);
                    }
                    responses::ProviderState::Stopping => job.step = Step::AwaitPlayniteStop,
                    responses::ProviderState::Starting | responses::ProviderState::Running => {
                        job.step = Step::StopPlaynite;
                    }
                    responses::ProviderState::CleanupFailed => {
                        return self.cleanup_failed(job, "playnite_stop_unconfirmed", send);
                    }
                }
            }
            Step::StopPlaynite => {
                if payload["ok"] == false && payload["code"] == "launch_not_found" {
                    job.step = Step::RecoverPlayniteStop;
                } else {
                    let provider = match responses::provider_launch(&payload, record) {
                        Ok(provider) => provider,
                        Err(code) => return self.cleanup_failed(job, code, send),
                    };
                    match provider.state {
                        responses::ProviderState::Stopped => {
                            record.provider_stopped = true;
                            job.step = first_cleanup_step(record);
                        }
                        responses::ProviderState::Stopping => {
                            job.step = Step::AwaitPlayniteStop;
                        }
                        _ => {
                            return self.cleanup_failed(job, "playnite_stop_unconfirmed", send);
                        }
                    }
                }
            }
            Step::AwaitPlayniteStop => {
                let provider = match responses::provider_launch(&payload, record) {
                    Ok(provider) => provider,
                    Err(code) => return self.cleanup_failed(job, code, send),
                };
                match provider.state {
                    responses::ProviderState::Stopped => {
                        record.provider_stopped = true;
                        job.step = first_cleanup_step(record);
                    }
                    responses::ProviderState::Stopping => {}
                    _ => return self.cleanup_failed(job, "playnite_stop_unconfirmed", send),
                }
            }
            Step::StopSession => {
                // Process termination is still attempted if the session provider
                // fails. Finish-stop must independently acknowledge lease release.
                if payload["ok"] != true {
                    record.cleanup_error = Some("session_stop_failed");
                }
                job.step = if record.process_id.is_some() {
                    Step::Terminate
                } else {
                    Step::Finish
                };
            }
            Step::Terminate | Step::AwaitTermination => {
                if payload["ok"] != true {
                    return self.cleanup_failed(job, "process_termination_failed", send);
                }
                let (_, state) = match responses::process(&payload, record) {
                    Ok(process) => process,
                    Err(code) => return self.cleanup_failed(job, code, send),
                };
                match state {
                    ApplicationState::Exited
                    | ApplicationState::LaunchFailed
                    | ApplicationState::Cancelled => job.step = Step::Finish,
                    ApplicationState::Stopping => job.step = Step::AwaitTermination,
                    ApplicationState::CleanupFailed(_) => {
                        return self.cleanup_failed(job, "application_cleanup_failed", send);
                    }
                    ApplicationState::Preparing | ApplicationState::Running => {
                        return self.cleanup_failed(job, "process_termination_failed", send);
                    }
                }
            }
            Step::Finish => {
                if payload["ok"] != true
                    || !responses::session(&payload, record).is_ok_and(|session| {
                        session["state"] == "stopped"
                            && session["leases"]
                                .as_object()
                                .is_some_and(|leases| leases.is_empty())
                    })
                {
                    return self.cleanup_failed(job, "session_finish_failed", send);
                }
                return self.finish(job, send);
            }
            _ => return Err("invalid launch cleanup workflow".into()),
        }
        self.retain(job)
    }
    fn deliver_activation(
        &mut self,
        job: &mut Workflow,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let request = job.request.ok_or("missing launch request")?;
        let active = matches!(send(Command::CheckRequest { request }), Ok(Response::Ok));
        let delivered = active
            && matches!(
                send(Command::Complete {
                    request,
                    payload: self.records.values[&job.launch_id].response(),
                }),
                Ok(Response::Ok)
            );
        if delivered {
            return Ok(());
        }
        job.begin_cleanup(
            self.records
                .values
                .get_mut(&job.launch_id)
                .expect("workflow record"),
            Outcome::Cancelled,
            Some("completion_unavailable"),
        );
        self.retain(job)
    }

    fn dependency_failed(
        &mut self,
        job: &mut Workflow,
        code: &'static str,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if job.cleanup {
            if job.step == Step::StopSession {
                let record = self
                    .records
                    .values
                    .get_mut(&job.launch_id)
                    .expect("workflow record");
                record.cleanup_error = Some(code);
                job.step = if record.process_id.is_some() {
                    Step::Terminate
                } else {
                    Step::Finish
                };
                return self.retain(job);
            }
            return self.cleanup_failed(job, code, send);
        }
        if job.step == Step::Monitor {
            // Loss of process visibility must not release its exclusive lease.
            return self.cleanup_failed(job, code, send);
        }
        let cancelled = job.request.is_some_and(|request| {
            !matches!(send(Command::CheckRequest { request }), Ok(Response::Ok))
        });
        job.begin_cleanup(
            self.records
                .values
                .get_mut(&job.launch_id)
                .expect("workflow record"),
            if cancelled {
                Outcome::Cancelled
            } else {
                Outcome::Failed
            },
            Some(if cancelled { "request_cancelled" } else { code }),
        );
        self.retain(job)
    }
    fn cleanup_failed(
        &mut self,
        job: &Workflow,
        code: &'static str,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let record = self
            .records
            .values
            .get_mut(&job.launch_id)
            .expect("workflow record");
        record.state = State::CleanupFailed;
        record.cleanup_error = Some(code);
        self.reply(job, send)
    }
    fn finish(
        &mut self,
        job: &Workflow,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let record = self
            .records
            .values
            .get_mut(&job.launch_id)
            .expect("workflow record");
        record.state = record.cleanup_outcome.state();
        record.cleanup_error = None;
        record.target = None;
        record.provider_ticket = None;
        self.reply(job, send)
    }
    fn reply(
        &self,
        job: &Workflow,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if let Some(request) = job.request {
            let record = &self.records.values[&job.launch_id];
            complete(
                send,
                request,
                if record.preflight_token.is_some() && !record.launch_started {
                    error(record.failure.unwrap_or("preflight_unavailable"))
                } else {
                    record.response()
                },
            )
        } else {
            Ok(())
        }
    }
    fn retain(&mut self, job: &mut Workflow) -> ModuleResult<()> {
        self.job = Some(job.clone());
        Ok(())
    }
}
fn complete(
    send: &mut impl FnMut(Command) -> Transport,
    request: u64,
    payload: Value,
) -> ModuleResult<()> {
    match send(Command::Complete { request, payload })? {
        Response::Ok => Ok(()),
        Response::Error { code, .. } if code == "request_ended" || code == "not_found" => Ok(()),
        _ => Err("launch completion failed".into()),
    }
}

fn is_monitored(record: &crate::records::Record) -> bool {
    record.state == State::Active
        && record
            .target
            .as_ref()
            .is_some_and(LaunchTarget::is_monitored_after_activation)
}

fn apply_application_outcome(record: &mut crate::records::Record, outcome: ApplicationOutcome) {
    record.cleanup_outcome = outcome.launch_outcome();
    match outcome {
        ApplicationOutcome::Exited => {}
        ApplicationOutcome::LaunchFailed => record.failure = Some("application_launch_failed"),
        ApplicationOutcome::Cancelled => record.failure = Some("application_cancelled"),
    }
}

#[cfg(test)]
mod tests;
