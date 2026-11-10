//! Exact coordinator admission validation shared by live teardown and shutdown.
use super::*;

pub(super) enum PreparationResolution {
    Pending,
    Rejected,
    Admitted,
    Unconfirmed,
}

pub(super) fn apply_response(record: &mut Record, payload: &Value) -> PreparationResolution {
    if refused_without_admission(payload) {
        // The coordinator keeps no receipt or work for a refused admission,
        // so no lifecycle stream exists to wait for. Leaving the preparation
        // outstanding made every later cleanup of this record poll for the
        // whole cleanup budget while holding the controller's only job.
        record.lifecycle_prepare_outstanding = false;
        return PreparationResolution::Rejected;
    }
    let operation = format!("{}-lifecycle-prepare", record.operation());
    if payload["operation_id"] != operation {
        return PreparationResolution::Unconfirmed;
    }
    if payload["status"] == "pending" {
        return PreparationResolution::Pending;
    }
    if payload["status"] == "rejected" {
        record.lifecycle_prepare_outstanding = false;
        return PreparationResolution::Rejected;
    }
    let target: Option<amaterasu_extension_api::LifecycleTarget> =
        serde_json::from_value(payload["target"].clone()).ok();
    let candidate: Option<amaterasu_extension_api::CaptureBinding> =
        serde_json::from_value(payload["candidate"].clone()).ok();
    let valid_target = match (&record.config.lifecycle_session_target, &target) {
        (
            Some(amaterasu_extension_api::LifecycleTarget::Session {
                host_epoch: expected_epoch,
                session_id: expected_session,
                ..
            }),
            Some(amaterasu_extension_api::LifecycleTarget::Stream {
                host_epoch,
                session_id,
                stream_id,
                generation,
            }),
        ) => {
            host_epoch == expected_epoch
                && session_id == expected_session
                && !stream_id.is_empty()
                && *generation > 0
        }
        _ => false,
    };
    // A failed prepare can have allocated a coordinator stream and already
    // rolled it back. No consumer was armed; the matching terminal receipt
    // settles preparation so local transport cleanup can finish.
    if payload["status"] == "failed" && payload["cleanup_completed"] == true && valid_target {
        record.lifecycle_prepare_outstanding = false;
        return PreparationResolution::Rejected;
    }
    let returned_mode = serde_json::from_value::<amaterasu_extension_api::CaptureBindingMode>(
        payload
            .get("capture_binding_mode")
            .cloned()
            .unwrap_or(json!("managed")),
    )
    .ok();
    let valid_candidate = if record.config.lifecycle_capture_mode.is_external() {
        payload["candidate"].is_null()
            && payload["required_consumers"] == json!(["capture"])
            && (record.config.lifecycle_intent.as_deref() != Some("join")
                || target.as_ref().is_some_and(|target| {
                    super::lifecycle_binding::valid_external_join(target, payload)
                }))
    } else if record.config.lifecycle_intent.as_deref() == Some("join") {
        candidate
            .as_ref()
            .zip(target.as_ref())
            .is_some_and(|(binding, target)| {
                super::lifecycle_binding::valid_join_binding(binding, target, payload)
            })
    } else {
        candidate
            .as_ref()
            .zip(record.negotiated_video)
            .zip(record.negotiated_video_stream_format)
            .is_some_and(|((binding, video), format)| {
                matches!(&target, Some(amaterasu_extension_api::LifecycleTarget::Stream { generation, .. }) if binding.generation == *generation)
                    && binding.geometry_revision > 0
                    && binding.plan_revision > 0
                    && binding.mode.width == video.width
                    && binding.mode.height == video.height
                    && binding.mode.refresh_millihz == video.fps * 1000
                    && binding.mode.hdr
                        == (format.dynamic_range == amaterasu_module_sdk::DynamicRange::Hdr)
            })
    };
    if payload["status"] != "succeeded"
        || !valid_target
        || !valid_candidate
        || returned_mode != Some(record.config.lifecycle_capture_mode)
    {
        return PreparationResolution::Unconfirmed;
    }
    let source = match preparation_source(record, &payload["target"], payload.get("shared_source"))
    {
        Some(source) => source,
        None => {
            return PreparationResolution::Unconfirmed;
        }
    };
    record.lifecycle_target = Some(payload["target"].clone());
    record.lifecycle_candidate = Some(payload["candidate"].clone());
    record.lifecycle_source_target = Some(source);
    record.lifecycle_prepare_outstanding = false;
    PreparationResolution::Admitted
}

/// A coordinator refusal carries neither an operation ID nor a target: it
/// admitted nothing. A refusal that names a target, or a recovery-required
/// reply, may follow an admitted effect and stays outstanding.
fn refused_without_admission(payload: &Value) -> bool {
    payload["status"] == "rejected"
        && payload.get("operation_id").is_none()
        && payload.get("target").is_none()
}
