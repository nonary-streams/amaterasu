//! Author-side adapters for processing lifecycle and intent providers.
//!
//! Consumers build these requests through `requests`; providers register and
//! decode them here, so a replacement provider shares one wire contract with
//! the built-in one. Provider-specific settings stay the provider's to validate.
use crate::{
    ApiFieldKind as Kind, ApiMethod, ApiPrivilege, ApiSchema,
    extension_api::{self, LifecycleTarget, ProviderOperation, ProviderRequest},
};
use serde_json::{Map, Value, json};

const INVALID_REQUEST: &str = "invalid_request";

fn schema(required: &[(&str, Kind)], optional: &[(&str, Kind)]) -> ApiSchema {
    let fields = |entries: &[(&str, Kind)]| {
        entries
            .iter()
            .cloned()
            .map(|(name, kind)| (name.into(), kind))
            .collect()
    };
    ApiSchema {
        required: fields(required),
        optional: fields(optional),
        allow_extra: false,
    }
}

fn lifecycle_request_schema() -> ApiSchema {
    schema(
        &[
            ("provider_sequence", Kind::Integer),
            ("operation", Kind::String),
            ("version", Kind::Integer),
            ("transition_id", Kind::String),
            ("operation_id", Kind::String),
            ("target", Kind::Object),
            ("phase", Kind::String),
            ("settings_revision", Kind::String),
            ("settings", Kind::Object),
            ("intent", Kind::Any),
            ("reason", Kind::Any),
            ("requested_video", Kind::Any),
            ("consumer_requirements", Kind::Object),
            ("remaining_ms", Kind::Integer),
            ("plan_revision", Kind::Any),
            ("admission_id", Kind::Any),
            ("approved_plan", Kind::Any),
        ],
        &[
            ("resource_generation", Kind::Integer),
            ("dependency_inputs", Kind::Array),
            ("control_results", Kind::Array),
            ("source_video_mode", Kind::Any),
            ("join_context", Kind::Object),
        ],
    )
}

fn lifecycle_status_schema() -> ApiSchema {
    schema(
        &[
            ("provider_sequence", Kind::Integer),
            ("transition_id", Kind::String),
            ("operation_id", Kind::String),
            ("receipt_id", Kind::String),
        ],
        &[],
    )
}

/// The plan, callback and status methods a lifecycle participant registers.
pub fn lifecycle_methods(service: &str) -> Vec<ApiMethod> {
    ["resource_plan", "lifecycle_callback", "lifecycle_status"]
        .into_iter()
        .map(|method| ApiMethod {
            service: service.into(),
            method: method.into(),
            interface_major: 1,
            interface_minor: 0,
            request: if method == "lifecycle_status" {
                lifecycle_status_schema()
            } else {
                lifecycle_request_schema()
            },
            response: schema(
                &[("status", Kind::String)],
                &[
                    ("reason", Kind::String),
                    ("plan", Kind::Object),
                    ("candidate", Kind::Any),
                    ("obligations", Kind::Array),
                    ("outputs", Kind::Array),
                    ("outcomes", Kind::Array),
                ],
            ),
            privilege: ApiPrivilege::User,
        })
        .collect()
}

/// The transport-neutral intent method a processing provider registers.
pub fn processing_intent_method(service: &str) -> ApiMethod {
    ApiMethod {
        service: service.into(),
        method: "processing_intent".into(),
        interface_major: 1,
        interface_minor: 1,
        request: schema(
            &[
                ("target", Kind::Object),
                ("revision", Kind::String),
                ("settings", Kind::Object),
            ],
            &[("output_hdr", Kind::Boolean)],
        ),
        response: schema(
            &[("status", Kind::String)],
            &[
                ("reason", Kind::String),
                ("target", Kind::Object),
                ("revision", Kind::String),
                ("enabled", Kind::Boolean),
                ("source_hdr_required", Kind::Any),
                ("output_hdr_required", Kind::Any),
                ("processor_settings", Kind::Object),
                ("processor_service", Kind::String),
            ],
        ),
        privilege: ApiPrivilege::User,
    }
}

/// One validated lifecycle delivery addressed to a participant.
#[derive(Debug, PartialEq)]
pub enum LifecycleDelivery {
    /// A receipt status query, whose arguments the participant does not need.
    Status,
    Request(Box<ProviderRequest>),
}

/// One validated processing intent. Settings are returned unvalidated because
/// only the provider knows its own settings descriptor.
#[derive(Debug, PartialEq)]
pub struct ProcessingIntent {
    pub target: LifecycleTarget,
    pub revision: String,
    pub settings: Value,
    pub output_hdr: Option<bool>,
}

pub(crate) fn valid_intent_revision(revision: &str) -> bool {
    !revision.is_empty() && revision.len() <= 128 && !revision.chars().any(char::is_control)
}

fn versioned_envelope(payload: &Value, minor: u32) -> Option<&Map<String, Value>> {
    payload
        .as_object()
        .filter(|envelope| envelope.len() == 4)
        .filter(|envelope| {
            envelope.get("interface_major") == Some(&json!(1))
                && envelope.get("interface_minor") == Some(&json!(minor))
        })
}

/// Decode a lifecycle delivery; the error is the wire rejection reason.
pub fn decode_lifecycle_delivery(payload: &Value) -> Result<LifecycleDelivery, &'static str> {
    let envelope = versioned_envelope(payload, 0).ok_or(INVALID_REQUEST)?;
    let expected = match envelope.get("method").and_then(Value::as_str) {
        Some("lifecycle_status") => return Ok(LifecycleDelivery::Status),
        Some("resource_plan") => ProviderOperation::ResourcePlan,
        Some("lifecycle_callback") => ProviderOperation::LifecycleCallback,
        _ => return Err("unknown_method"),
    };
    let arguments = envelope.get("arguments").ok_or(INVALID_REQUEST)?;
    let request: ProviderRequest =
        serde_json::from_value(arguments.clone()).map_err(|_| INVALID_REQUEST)?;
    if request.operation != expected || extension_api::validate_provider_request(&request).is_err()
    {
        return Err(INVALID_REQUEST);
    }
    Ok(LifecycleDelivery::Request(Box::new(request)))
}

/// Decode a processing intent; the error is the wire rejection reason.
pub fn decode_processing_intent(payload: &Value) -> Result<ProcessingIntent, &'static str> {
    let envelope = versioned_envelope(payload, 1)
        .filter(|envelope| envelope.get("method") == Some(&json!("processing_intent")))
        .ok_or(INVALID_REQUEST)?;
    let arguments = envelope
        .get("arguments")
        .and_then(Value::as_object)
        .filter(|arguments| {
            arguments.len() == 3
                || (arguments.len() == 4
                    && arguments.get("output_hdr").is_some_and(Value::is_boolean))
        })
        .ok_or(INVALID_REQUEST)?;
    let target = arguments.get("target").ok_or(INVALID_REQUEST)?;
    let target = serde_json::from_value(target.clone()).map_err(|_| "invalid_target")?;
    let revision = arguments
        .get("revision")
        .and_then(Value::as_str)
        .filter(|revision| valid_intent_revision(revision))
        .ok_or("invalid_revision")?;
    let settings = arguments.get("settings").ok_or("invalid_settings")?;
    Ok(ProcessingIntent {
        target,
        revision: revision.to_owned(),
        settings: settings.clone(),
        output_hdr: arguments.get("output_hdr").and_then(Value::as_bool),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Value {
        json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1})
    }

    fn intent_envelope(arguments: Value) -> Value {
        json!({"method":"processing_intent","interface_major":1,"interface_minor":1,"arguments":arguments})
    }

    #[test]
    fn lifecycle_status_needs_no_arguments_but_requires_the_interface() {
        let status = json!({"method":"lifecycle_status","interface_major":1,"interface_minor":0,"arguments":{}});
        assert_eq!(
            decode_lifecycle_delivery(&status),
            Ok(LifecycleDelivery::Status)
        );
        let mut wrong_minor = status;
        wrong_minor["interface_minor"] = json!(1);
        assert_eq!(
            decode_lifecycle_delivery(&wrong_minor),
            Err(INVALID_REQUEST)
        );
    }

    #[test]
    fn lifecycle_rejects_unknown_methods_and_malformed_arguments() {
        let envelope = |method: &str| json!({"method":method,"interface_major":1,"interface_minor":0,"arguments":{}});
        assert_eq!(
            decode_lifecycle_delivery(&envelope("other")),
            Err("unknown_method")
        );
        assert_eq!(
            decode_lifecycle_delivery(&envelope("resource_plan")),
            Err(INVALID_REQUEST)
        );
    }

    #[test]
    fn intent_preserves_the_output_range_and_echoes_a_typed_target() {
        let decoded = decode_processing_intent(&intent_envelope(json!({
            "target":target(),"revision":"r","settings":{"enabled":true},"output_hdr":true
        })))
        .unwrap();
        assert_eq!(json!(decoded.target), target());
        assert_eq!(decoded.output_hdr, Some(true));
        let unknown = decode_processing_intent(&intent_envelope(json!({
            "target":target(),"revision":"r","settings":{}
        })))
        .unwrap();
        assert_eq!(unknown.output_hdr, None);
    }

    #[test]
    fn intent_reports_each_invalid_field_with_its_wire_reason() {
        let cases = [
            (
                json!({"target":{"kind":"none"},"revision":"r","settings":{}}),
                "invalid_target",
            ),
            (
                json!({"target":target(),"revision":"","settings":{}}),
                "invalid_revision",
            ),
            (
                json!({"target":target(),"revision":"r","other":{}}),
                "invalid_settings",
            ),
            (
                json!({"target":target(),"revision":"r","settings":{},"output_hdr":"yes"}),
                INVALID_REQUEST,
            ),
        ];
        for (arguments, reason) in cases {
            assert_eq!(
                decode_processing_intent(&intent_envelope(arguments)),
                Err(reason)
            );
        }
        let mut old_version =
            intent_envelope(json!({"target":target(),"revision":"r","settings":{}}));
        old_version["interface_minor"] = json!(0);
        assert_eq!(decode_processing_intent(&old_version), Err(INVALID_REQUEST));
    }
}
