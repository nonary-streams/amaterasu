//! Validate dependency reply identities before adopting IDs or advancing ownership.
use crate::protocol::{ApplicationOutcome, ApplicationState, LaunchTarget};
use crate::records::Record;
use serde_json::Value;

pub fn session<'a>(payload: &'a Value, record: &Record) -> Result<&'a Value, &'static str> {
    let sessions = payload["sessions"]
        .as_array()
        .filter(|sessions| sessions.len() == 1)
        .ok_or("invalid_session_response")?;
    let session = &sessions[0];
    let id = session["id"]
        .as_u64()
        .filter(|&id| id > 0)
        .ok_or("invalid_session_response")?;
    if session["key"] != record.session_key
        || record.session_id.is_some_and(|expected| expected != id)
    {
        return Err("invalid_session_response");
    }
    Ok(session)
}
pub fn process<'a>(
    payload: &'a Value,
    record: &Record,
) -> Result<(&'a Value, ApplicationState), &'static str> {
    let process = &payload["process"];
    let id = process["process_id"]
        .as_u64()
        .filter(|&id| id > 0)
        .ok_or("invalid_application_response")?;
    if process["operation_id"] != record.process_operation
        || record.process_id.is_some_and(|expected| expected != id)
    {
        return Err("invalid_application_response");
    }
    let state = match process["state"].as_str() {
        Some("preparing") => ApplicationState::Preparing,
        Some("running") => ApplicationState::Running,
        Some("stopping") => ApplicationState::Stopping,
        Some("cleanup_failed") => {
            ApplicationState::CleanupFailed(match process["outcome"].as_str() {
                Some("exited") => ApplicationOutcome::Exited,
                Some("launch_failed") => ApplicationOutcome::LaunchFailed,
                Some("cancelled") => ApplicationOutcome::Cancelled,
                _ => return Err("invalid_application_response"),
            })
        }
        Some("exited") => ApplicationState::Exited,
        Some("launch_failed") => ApplicationState::LaunchFailed,
        Some("cancelled") => ApplicationState::Cancelled,
        _ => return Err("invalid_application_response"),
    };
    if !matches!(state, ApplicationState::CleanupFailed(_)) && process.get("outcome").is_some() {
        return Err("invalid_application_response");
    }
    Ok((process, state))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderState {
    Starting,
    Running,
    Stopping,
    Stopped,
    LaunchFailed,
    CleanupFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreparationState {
    Preparing,
    Ready,
    Cleaning,
    PreparationFailed,
    CleanupFailed,
    Complete,
}

pub fn preparation<'a>(
    payload: &'a Value,
    record: &Record,
) -> Result<(&'a Value, PreparationState), &'static str> {
    let object = payload
        .as_object()
        .filter(|object| object.len() == 2 && object.contains_key("ok"))
        .ok_or("invalid_preparation_response")?;
    if payload["ok"] != true || object.get("preparation").is_none() {
        return Err("invalid_preparation_response");
    }
    let preparation = &payload["preparation"];
    let lease = preparation
        .as_object()
        .filter(|lease| {
            lease.len() == 4
                && lease.contains_key("operation_id")
                && lease.contains_key("state")
                && lease.contains_key("preparation")
                && lease.contains_key("cleanup")
        })
        .ok_or("invalid_preparation_response")?;
    if lease["operation_id"] != record.preparation_operation
        || !lease["preparation"].is_object()
        || !lease["cleanup"].is_object()
    {
        return Err("invalid_preparation_response");
    }
    let state = match lease["state"].as_str() {
        Some("preparing") => PreparationState::Preparing,
        Some("ready") => PreparationState::Ready,
        Some("cleaning") => PreparationState::Cleaning,
        Some("preparation_failed") => PreparationState::PreparationFailed,
        Some("cleanup_failed") => PreparationState::CleanupFailed,
        Some("complete") => PreparationState::Complete,
        _ => return Err("invalid_preparation_response"),
    };
    Ok((preparation, state))
}

pub struct ProviderLaunch {
    pub ticket: String,
    pub state: ProviderState,
}

pub fn provider_start_not_started(payload: &Value) -> bool {
    let Some(object) = payload.as_object() else {
        return false;
    };
    object.len() == 3
        && object
            .keys()
            .all(|key| matches!(key.as_str(), "ok" | "code" | "launch_state"))
        && payload["ok"] == false
        && payload["code"]
            .as_str()
            .is_some_and(|code| !code.is_empty())
        && payload["launch_state"] == "not_started"
}

pub fn provider_lookup_not_found(payload: &Value, record: &Record) -> bool {
    if !matches!(
        record.target.as_ref(),
        Some(LaunchTarget::PlayniteFullscreen { .. })
    ) {
        return false;
    }
    let Some(object) = payload.as_object() else {
        return false;
    };
    object.len() == 2
        && object
            .keys()
            .all(|key| matches!(key.as_str(), "ok" | "found"))
        && payload["ok"] == true
        && payload["found"] == false
}

pub fn provider_launch(payload: &Value, record: &Record) -> Result<ProviderLaunch, &'static str> {
    if payload["ok"] != true {
        return Err("invalid_provider_response");
    }
    let target = record.target.as_ref().ok_or("invalid_provider_response")?;
    let ticket = payload["ticket"]
        .as_str()
        .filter(|ticket| {
            ticket.len() == 48
                && ticket
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or("invalid_provider_response")?;
    let identity_matches = match target {
        LaunchTarget::Playnite { game_id, .. } => {
            let response_game_id = payload["game_id"]
                .as_str()
                .filter(|value| value.eq_ignore_ascii_case(game_id))
                .ok_or("invalid_provider_response")?;
            response_game_id.to_ascii_lowercase() == *game_id
        }
        LaunchTarget::PlayniteFullscreen { .. } => {
            (payload["started"] == true && payload.get("found").is_none())
                || (payload["found"] == true && payload.get("started").is_none())
        }
        LaunchTarget::Executable(_) => false,
    };
    if !identity_matches
        || record
            .provider_ticket
            .as_deref()
            .is_some_and(|expected| expected != ticket)
    {
        return Err("invalid_provider_response");
    }
    let state = match payload["state"].as_str() {
        Some("starting") => ProviderState::Starting,
        Some("running") => ProviderState::Running,
        Some("stopping") => ProviderState::Stopping,
        Some("stopped") => ProviderState::Stopped,
        Some("launch_failed") => ProviderState::LaunchFailed,
        Some("cleanup_failed") => ProviderState::CleanupFailed,
        _ => return Err("invalid_provider_response"),
    };
    let object = payload.as_object().ok_or("invalid_provider_response")?;
    let expected_keys: &[&str] = match target {
        LaunchTarget::Playnite { .. } => &["ok", "ticket", "game_id", "state"],
        LaunchTarget::PlayniteFullscreen { .. } if payload.get("started").is_some() => {
            &["ok", "started", "ticket", "state"]
        }
        LaunchTarget::PlayniteFullscreen { .. } => &["ok", "found", "ticket", "state"],
        LaunchTarget::Executable(_) => return Err("invalid_provider_response"),
    };
    if object.len() != expected_keys.len()
        || object
            .keys()
            .any(|key| !expected_keys.contains(&key.as_str()))
    {
        return Err("invalid_provider_response");
    }
    Ok(ProviderLaunch {
        ticket: ticket.into(),
        state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::{CatalogSelection, GlobalPreparation, Records};
    use serde_json::json;

    fn record() -> (Records, u64) {
        let mut records = Records::new().unwrap();
        let (id, _) = records
            .start(
                "owner",
                "owner",
                "operation".into(),
                "app-0000000000000001".into(),
                CatalogSelection::Initiator,
                GlobalPreparation::default(),
            )
            .unwrap();
        records.values.get_mut(&id).unwrap().process_id = Some(3);
        (records, id)
    }

    #[test]
    fn process_accepts_intermediate_terminal_and_cleanup_failed_states() {
        let (records, id) = record();
        for (state, outcome, expected) in [
            ("preparing", None, ApplicationState::Preparing),
            ("running", None, ApplicationState::Running),
            ("stopping", None, ApplicationState::Stopping),
            ("exited", None, ApplicationState::Exited),
            ("launch_failed", None, ApplicationState::LaunchFailed),
            ("cancelled", None, ApplicationState::Cancelled),
            (
                "cleanup_failed",
                Some("launch_failed"),
                ApplicationState::CleanupFailed(ApplicationOutcome::LaunchFailed),
            ),
        ] {
            let mut payload = json!({"process":{
                "process_id":3,
                "operation_id":records.values[&id].process_operation,
                "state":state,
            }});
            if let Some(outcome) = outcome {
                payload["process"]["outcome"] = json!(outcome);
            }
            assert_eq!(process(&payload, &records.values[&id]).unwrap().1, expected);
        }
    }

    #[test]
    fn cleanup_failed_requires_a_known_outcome_and_other_states_reject_it() {
        let (records, id) = record();
        for payload in [
            json!({"process":{"process_id":3,"operation_id":records.values[&id].process_operation,"state":"cleanup_failed"}}),
            json!({"process":{"process_id":3,"operation_id":records.values[&id].process_operation,"state":"cleanup_failed","outcome":"unknown"}}),
            json!({"process":{"process_id":3,"operation_id":records.values[&id].process_operation,"state":"running","outcome":"exited"}}),
        ] {
            assert_eq!(
                process(&payload, &records.values[&id]).unwrap_err(),
                "invalid_application_response"
            );
        }
    }

    #[test]
    fn a_preflight_rejection_proves_no_start_only_with_the_exact_marker_shape() {
        assert!(provider_start_not_started(
            &json!({"ok":false,"code":"playnite_disabled","launch_state":"not_started"})
        ));
        for payload in [
            json!({"ok":false,"code":"playnite_disabled"}),
            json!({"ok":false,"code":"playnite_disabled","launch_state":"unknown"}),
            json!({"ok":false,"code":"playnite_disabled","launch_state":"not_started","ticket":"ab".repeat(24)}),
            json!({"ok":true,"code":"playnite_disabled","launch_state":"not_started"}),
            json!({"ok":false,"launch_state":"not_started"}),
        ] {
            assert!(!provider_start_not_started(&payload), "{payload}");
        }
    }
}
