//! Identity-bound provider responses for a future generic launch workflow.
//!
//! These checks authenticate the shape and correlation of assertions made by a
//! trusted provider. They are not cryptographic proof of operating-system state.
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityResponse {
    ok: bool,
    provider_incarnation: String,
}

/// Extract the provider incarnation before submitting any effect-bearing start.
pub fn provider_incarnation(payload: &serde_json::Value) -> Result<String, &'static str> {
    let response: IdentityResponse =
        serde_json::from_value(payload.clone()).map_err(|_| "invalid_provider_response")?;
    if !response.ok || !valid_token(&response.provider_incarnation) {
        return Err("invalid_provider_response");
    }
    Ok(response.provider_incarnation)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    ok: bool,
    operation_id: String,
    application_id: String,
    provider_incarnation: String,
    #[serde(deserialize_with = "required_option")]
    ticket: Option<String>,
    state: State,
    #[serde(deserialize_with = "required_option")]
    retirement_proof: Option<RetirementProof>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Starting,
    Running,
    Stopping,
    Stopped,
    Unknown,
    LaunchFailed,
    CleanupFailed,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetirementProof {
    kind: String,
    operation_id: String,
    application_id: String,
    provider_incarnation: String,
    #[serde(deserialize_with = "required_option")]
    ticket: Option<String>,
}

fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

/// Values captured by launch before submitting an effect-bearing start.
pub struct Expected<'a> {
    pub operation_id: &'a str,
    pub application_id: &'a str,
    pub provider_incarnation: &'a str,
    pub ticket: Option<&'a str>,
}

/// A correlated response; only `retired` authorizes releasing owned resources.
#[derive(Debug, Eq, PartialEq)]
pub struct Evidence {
    pub state: State,
    pub ticket: Option<String>,
    pub retired: bool,
}

/// Reject changed identity, malformed handles, and terminal claims without a
/// matching provider fence. Unknown and cleanup failure never prove retirement.
pub fn validate(
    payload: &serde_json::Value,
    expected: &Expected<'_>,
) -> Result<Evidence, &'static str> {
    let response: Response =
        serde_json::from_value(payload.clone()).map_err(|_| "invalid_provider_response")?;
    if !response.ok
        || response.operation_id != expected.operation_id
        || response.application_id != expected.application_id
        || response.provider_incarnation != expected.provider_incarnation
        || !valid_token(&response.provider_incarnation)
        || response
            .ticket
            .as_deref()
            .is_some_and(|ticket| !valid_token(ticket))
        || expected
            .ticket
            .is_some_and(|ticket| response.ticket.as_deref() != Some(ticket))
        || matches!(
            response.state,
            State::Starting | State::Running | State::Stopping
        ) && response.ticket.is_none()
    {
        return Err("invalid_provider_response");
    }
    let terminal = matches!(response.state, State::Stopped | State::LaunchFailed);
    let retired = match response.retirement_proof {
        Some(proof)
            if terminal
                && proof.kind == "fenced_stopped"
                && proof.operation_id == response.operation_id
                && proof.application_id == response.application_id
                && proof.provider_incarnation == response.provider_incarnation
                && proof.ticket == response.ticket =>
        {
            true
        }
        None if !terminal => false,
        _ => return Err("invalid_provider_response"),
    };
    Ok(Evidence {
        state: response.state,
        ticket: response.ticket,
        retired,
    })
}

fn valid_token(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn expected() -> Expected<'static> {
        Expected {
            operation_id: "op",
            application_id: "app",
            provider_incarnation: "epoch",
            ticket: Some("ticket"),
        }
    }

    fn stopped() -> serde_json::Value {
        json!({"ok":true,"operation_id":"op","application_id":"app","provider_incarnation":"epoch","ticket":"ticket","state":"stopped","retirement_proof":{"kind":"fenced_stopped","operation_id":"op","application_id":"app","provider_incarnation":"epoch","ticket":"ticket"}})
    }

    #[test]
    fn terminal_retirement_requires_all_identity_fields_and_a_fence() {
        assert!(validate(&stopped(), &expected()).unwrap().retired);
        for pointer in [
            "/operation_id",
            "/application_id",
            "/provider_incarnation",
            "/ticket",
            "/retirement_proof/operation_id",
            "/retirement_proof/application_id",
            "/retirement_proof/provider_incarnation",
            "/retirement_proof/ticket",
            "/retirement_proof/kind",
        ] {
            let mut payload = stopped();
            *payload.pointer_mut(pointer).unwrap() = json!("other");
            assert!(validate(&payload, &expected()).is_err(), "{pointer}");
        }
        let mut missing_field = stopped();
        missing_field.as_object_mut().unwrap().remove("ticket");
        assert!(validate(&missing_field, &expected()).is_err());
        let mut missing = stopped();
        missing["retirement_proof"] = json!(null);
        assert!(validate(&missing, &expected()).is_err());
    }

    #[test]
    fn unknown_is_never_retirement_and_changed_incarnation_is_rejected() {
        let mut payload = stopped();
        payload["state"] = json!("unknown");
        payload["retirement_proof"] = json!(null);
        assert!(!validate(&payload, &expected()).unwrap().retired);
        payload["provider_incarnation"] = json!("new-epoch");
        assert!(validate(&payload, &expected()).is_err());
    }

    #[test]
    fn identity_must_be_nonempty_and_bounded() {
        assert_eq!(
            provider_incarnation(&json!({"ok":true,"provider_incarnation":"epoch"})).unwrap(),
            "epoch"
        );
        for token in ["", "has space", &"x".repeat(129)] {
            assert!(
                provider_incarnation(&json!({"ok":true,"provider_incarnation":token})).is_err()
            );
        }
    }
}
