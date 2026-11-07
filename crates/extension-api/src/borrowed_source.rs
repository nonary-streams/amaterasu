//! Frozen context for a source retained by an independently selected provider.
//!
//! The trusted transport verifies and reserves the source through its selected
//! provider before requesting lifecycle preparation. These facts identify that
//! existing reservation; they never grant native mutation or prove readiness.
use crate::{ProviderContext, VideoMode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Reserved coordinator-owned field in `ProviderContext.application`.
pub const BORROWED_CAPTURE_CONTEXT_KEY: &str = "amaterasu.borrowed_capture/1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BorrowedCaptureContext {
    /// Identity derived from authenticated transport, never a query parameter.
    pub client_id: String,
    /// The source provider's opaque session identity and retained generation.
    pub session_id: String,
    pub generation: u64,
    /// Provider-issued provisional hold, acquired before consumer effects.
    pub reservation_id: String,
    /// Exact consumer operation receiving the provisional source hold.
    pub transport_id: String,
    pub transport_epoch: String,
    /// Logical selector resolved by the installed capture provider.
    pub output: String,
    /// Opaque route selected by trusted startup composition. Changing this key
    /// changes the retained source even when its logical output is unchanged.
    #[serde(default = "default_route", skip_serializing_if = "is_default_route")]
    pub route_key: String,
    /// The retained source mode, distinct from the requested stream mode.
    pub mode: VideoMode,
}

impl BorrowedCaptureContext {
    pub fn validate(&self) -> Result<(), String> {
        if ![
            &self.client_id,
            &self.session_id,
            &self.reservation_id,
            &self.transport_id,
            &self.transport_epoch,
        ]
        .into_iter()
        .all(|id| valid_opaque_id(id))
            || self.generation == 0
            || !valid_opaque_id(&self.route_key)
            || self.output.is_empty()
            || self.output.len() > 512
            || self.output.chars().any(char::is_control)
            || matches!(
                self.output.as_str(),
                "amaterasu-owned-display" | "portal-selected-session-source"
            )
            || !(1..=16_384).contains(&self.mode.width)
            || !(1..=16_384).contains(&self.mode.height)
            || !(1..=1_000_000).contains(&self.mode.refresh_millihz)
        {
            return Err("invalid borrowed capture context".into());
        }
        Ok(())
    }
}

fn default_route() -> String {
    "default".into()
}

fn is_default_route(route: &str) -> bool {
    route == "default"
}

/// Read context only from the authenticated lifecycle coordinator response.
/// Callers must retain their normal provider-request authorization checks.
pub fn borrowed_capture_context(
    context: &ProviderContext,
) -> Result<Option<BorrowedCaptureContext>, String> {
    borrowed_capture_from_application(&context.application)
}

pub fn borrowed_capture_from_application(
    application: &Value,
) -> Result<Option<BorrowedCaptureContext>, String> {
    let Some(value) = application.get(BORROWED_CAPTURE_CONTEXT_KEY) else {
        return Ok(None);
    };
    let context: BorrowedCaptureContext = serde_json::from_value(value.clone())
        .map_err(|_| "invalid borrowed capture context".to_owned())?;
    context.validate()?;
    Ok(Some(context))
}

fn valid_opaque_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source() -> BorrowedCaptureContext {
        BorrowedCaptureContext {
            client_id: "authenticated-client".into(),
            session_id: "retained-output".into(),
            generation: 7,
            reservation_id: "source-hold".into(),
            transport_id: "transport-operation".into(),
            transport_epoch: "transport-host-epoch".into(),
            output: "exact-target:logical-selector".into(),
            route_key: default_route(),
            mode: VideoMode {
                width: 2560,
                height: 1440,
                refresh_millihz: 144_000,
                hdr: true,
            },
        }
    }

    #[test]
    fn frozen_source_mode_does_not_masquerade_as_the_stream_mode() {
        let mut application = json!({"kind":"application","app_id":"app-1"});
        application[BORROWED_CAPTURE_CONTEXT_KEY] = json!(source());
        let context = ProviderContext {
            target: serde_json::from_value(json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1})).unwrap(),
            application,
            requested_video: Some(VideoMode {
                width: 1920,
                height: 1080,
                refresh_millihz: 60_000,
                hdr: false,
            }),
            client: None,
        };
        assert_eq!(borrowed_capture_context(&context).unwrap(), Some(source()));
        assert_ne!(context.requested_video, Some(source().mode));
    }

    #[test]
    fn malformed_and_unbounded_source_claims_fail_closed() {
        for field in [
            "client_id",
            "session_id",
            "reservation_id",
            "transport_id",
            "transport_epoch",
            "route_key",
        ] {
            let mut value = json!(source());
            value[field] = json!("x".repeat(129));
            let mut application = json!({});
            application[BORROWED_CAPTURE_CONTEXT_KEY] = value;
            assert!(borrowed_capture_from_application(&application).is_err());
        }
        for value in [
            json!({"generation":0}),
            json!({"output":"amaterasu-owned-display"}),
            json!({"output":"portal-selected-session-source"}),
            json!({"mode":{"width":0,"height":1080,"refresh_millihz":60000,"hdr":false}}),
            json!({"extra":"unsupported"}),
        ] {
            let mut claimed = json!(source());
            claimed
                .as_object_mut()
                .unwrap()
                .extend(value.as_object().unwrap().clone());
            let mut application = json!({});
            application[BORROWED_CAPTURE_CONTEXT_KEY] = claimed;
            assert!(borrowed_capture_from_application(&application).is_err());
        }
        assert_eq!(borrowed_capture_from_application(&json!({})).unwrap(), None);
    }

    #[test]
    fn omitted_route_preserves_legacy_default_and_explicit_routes_round_trip() {
        let legacy = serde_json::to_value(source()).unwrap();
        assert!(legacy.get("route_key").is_none());
        let decoded: BorrowedCaptureContext = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(decoded.route_key, "default");
        decoded.validate().unwrap();
        let mut explicit = legacy;
        explicit["route_key"] = json!("retained-monitor-route");
        let decoded: BorrowedCaptureContext = serde_json::from_value(explicit.clone()).unwrap();
        decoded.validate().unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), explicit);
        for invalid in ["", "route\nchanged"] {
            explicit["route_key"] = json!(invalid);
            assert!(
                serde_json::from_value::<BorrowedCaptureContext>(explicit.clone())
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
    }
}
