//! Bounded warning records for optional stream features.
//!
//! Reporting a warning is best-effort and must never decide stream admission.
//! Values are restricted to scalars so a provider cannot accidentally log an
//! arbitrary request body or other sensitive data.
use crate::{Command, ModuleResult, requests::RequestOptions};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub const MAX_WARNINGS: usize = 16;
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WarningValue {
    Boolean(bool),
    Integer(i64),
    Label(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamWarning {
    pub code: String,
    pub source: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_error: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested: Option<WarningValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective: Option<WarningValue>,
}

impl StreamWarning {
    pub fn valid(&self) -> bool {
        identifier(&self.code, 64)
            && identifier(&self.source, 128)
            && identifier(&self.reason, 64)
            && [&self.requested, &self.effective]
                .into_iter()
                .all(|value| match value {
                    Some(WarningValue::Label(label)) => identifier(label, 64),
                    Some(WarningValue::Integer(value)) => {
                        (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(value)
                    }
                    _ => true,
                })
    }
}

/// Request associated with one exact, already-reserved stream history identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordHistoryWarnings {
    pub activity_id: String,
    pub rtsp_operation_id: String,
    pub warnings: Vec<StreamWarning>,
}

impl RecordHistoryWarnings {
    pub fn valid(&self) -> bool {
        identifier(&self.activity_id, 128)
            && self.activity_id == self.rtsp_operation_id
            && !self.warnings.is_empty()
            && self.warnings.len() <= MAX_WARNINGS
            && self.warnings.iter().all(StreamWarning::valid)
    }
}

pub fn record_history_warnings(
    options: RequestOptions<'_>,
    request: &RecordHistoryWarnings,
) -> ModuleResult<Command> {
    if !identifier(options.service, 128)
        || !identifier(options.operation_id, 128)
        || !(1..=30_000).contains(&options.timeout_ms)
        || options.parent == Some(0)
        || !request.valid()
    {
        return Err("invalid stream warning request".into());
    }
    Ok(Command::Request {
        service: options.service.into(),
        payload: json!({"action":"record_history_warnings", "activity_id":request.activity_id,
            "rtsp_operation_id":request.rtsp_operation_id, "warnings":request.warnings}),
        timeout_ms: options.timeout_ms,
        operation_id: options.operation_id.into(),
        parent: options.parent,
    })
}

/// Optional read-only warning query. An older or replacement launch provider
/// may decline it; callers must continue without stream history in that case.
pub fn read_launch_warnings(options: RequestOptions<'_>, launch_id: u64) -> ModuleResult<Command> {
    if !identifier(options.service, 128)
        || !identifier(options.operation_id, 128)
        || !(1..=30_000).contains(&options.timeout_ms)
        || options.parent == Some(0)
        || launch_id == 0
    {
        return Err("invalid launch warning query".into());
    }
    Ok(Command::Request {
        service: options.service.into(),
        payload: json!({"action":"warnings", "launch_id":launch_id}),
        timeout_ms: options.timeout_ms,
        operation_id: options.operation_id.into(),
        parent: options.parent,
    })
}

pub fn parse_launch_warnings(
    payload: &serde_json::Value,
    launch_id: u64,
    launch_operation: &str,
) -> Option<Vec<StreamWarning>> {
    if payload["ok"] != true
        || payload["launch_id"] != launch_id
        || payload["operation_id"] != launch_operation
    {
        return None;
    }
    let warnings: Vec<StreamWarning> =
        serde_json::from_value(payload.get("warnings")?.clone()).ok()?;
    (warnings.len() <= MAX_WARNINGS && warnings.iter().all(StreamWarning::valid))
        .then_some(warnings)
}

fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-/:".contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_records_are_bounded_and_bind_the_exact_history_owner() {
        let warning = StreamWarning {
            code: "optional_policy_unavailable".into(),
            source: "amaterasu.nvhttp/1".into(),
            reason: "read_unavailable".into(),
            os_error: None,
            requested: None,
            effective: Some(WarningValue::Boolean(false)),
        };
        let mut request = RecordHistoryWarnings {
            activity_id: "rtsp-123".into(),
            rtsp_operation_id: "rtsp-123".into(),
            warnings: vec![warning],
        };
        assert!(request.valid());
        assert_eq!(
            serde_json::from_value::<RecordHistoryWarnings>(json!(request)).unwrap(),
            request
        );
        request.rtsp_operation_id = "other".into();
        assert!(!request.valid());
        request.rtsp_operation_id = request.activity_id.clone();
        request.warnings[0].reason = "private payload with spaces".into();
        assert!(!request.valid());
        request.warnings[0].reason = "unavailable".into();
        request.warnings[0].requested = Some(WarningValue::Integer(MAX_SAFE_INTEGER + 1));
        assert!(!request.valid());
        request.warnings[0].requested = Some(WarningValue::Integer(MAX_SAFE_INTEGER));
        assert!(request.valid());
        request
            .warnings
            .resize(MAX_WARNINGS + 1, request.warnings[0].clone());
        assert!(!request.valid());
    }

    #[test]
    fn launch_warning_query_rejects_foreign_or_malformed_records() {
        let response = json!({"ok":true,"launch_id":7,"operation_id":"app-op",
            "warnings":[{"code":"optional_feature_unavailable","source":"example/1",
                "reason":"unavailable","effective":false}]});
        assert_eq!(
            parse_launch_warnings(&response, 7, "app-op").unwrap().len(),
            1
        );
        assert!(parse_launch_warnings(&response, 8, "app-op").is_none());
        assert!(parse_launch_warnings(&response, 7, "other").is_none());
        let mut malformed = response;
        malformed["warnings"][0]["reason"] = json!("untrusted raw payload");
        assert!(parse_launch_warnings(&malformed, 7, "app-op").is_none());
    }
}
