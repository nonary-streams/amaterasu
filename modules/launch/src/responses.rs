//! Validate dependency reply identities before adopting IDs or advancing ownership.
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
pub fn process<'a>(payload: &'a Value, record: &Record) -> Result<&'a Value, &'static str> {
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
    if !matches!(
        process["state"].as_str(),
        Some("running" | "exited" | "launch_failed")
    ) {
        return Err("invalid_application_response");
    }
    Ok(process)
}
