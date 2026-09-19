//! Validate dependency reply identities before adopting IDs or advancing ownership.
use crate::protocol::{ApplicationOutcome, ApplicationState};
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::{CatalogSelection, Records};
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
}
