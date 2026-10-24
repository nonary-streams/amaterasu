//! A stream owns one monitor identity; capture closes before connector removal.
use super::{record::Record, rpc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(super) const REQUESTED_OUTPUT: &str = "amaterasu-owned-display";
pub(super) fn requested(record: &Record) -> bool {
    record
        .start
        .as_ref()
        .is_some_and(|start| start.output == REQUESTED_OUTPUT)
}
pub(super) fn identity(record: &Record) -> String {
    let mut bytes = Sha256::digest(format!("amaterasu-stream-display-{:016x}", record.id));
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}
pub(super) fn create(record: &Record) -> Value {
    let configuration = &record
        .start
        .as_ref()
        .expect("start precedes virtual display allocation")
        .configuration;
    json!({"action":"create","identity":identity(record),"width":configuration.width,"height":configuration.height,
        "refresh_millihz":configuration.fps*1000,"name":"Amaterasu"})
}
pub(super) fn status(record: &Record) -> Value {
    json!({"action":"status","identity":identity(record)})
}
pub(super) fn remove(record: &Record) -> Value {
    json!({"action":"remove","identity":identity(record)})
}
pub(super) fn accept_ready(record: &mut Record, payload: &Value) -> Result<bool, &'static str> {
    if payload["ok"] != true {
        return Err("virtual_display_status_unavailable");
    }
    let monitors = payload["monitors"]
        .as_array()
        .ok_or("invalid_virtual_display_response")?;
    if monitors.len() != 1 {
        return Err("virtual_display_missing");
    }
    let monitor = &monitors[0];
    if monitor["specification"]["identity"] != identity(record) {
        return Err("invalid_virtual_display_identity");
    }
    match monitor["state"].as_str() {
        Some("creating") => Ok(false),
        Some("ready") => {
            let output = monitor["capture_output"]
                .as_str()
                .filter(|output| (1..=8).any(|index| *output == format!("kwin:Virtual-{index}")))
                .ok_or("invalid_virtual_display_output")?;
            let configuration = &record
                .start
                .as_ref()
                .ok_or("virtual_display_start_missing")?
                .configuration;
            if monitor["specification"]["width"] != configuration.width
                || monitor["specification"]["height"] != configuration.height
                || monitor["specification"]["refresh_millihz"] != configuration.fps * 1000
                || monitor["ready"]["active"] != true
            {
                return Err("virtual_display_configuration_mismatch");
            }
            record.virtual_display_output = Some(output.into());
            Ok(true)
        }
        _ => Err("virtual_display_not_ready"),
    }
}
pub(super) fn absent(record: &Record, payload: &Value) -> bool {
    payload["ok"] == true
        && (payload["removed"] == true && payload["identity"] == identity(record)
            || payload["monitors"]
                .as_array()
                .is_some_and(|monitors| monitors.is_empty()))
}
pub(super) fn shutdown(record: &Record, send: &mut rpc::Send<'_>) -> bool {
    if !record.virtual_display_attempted {
        return true;
    }
    let mut pending = rpc::Pending::new(
        "amaterasu.virtual-display/1",
        remove(record),
        format!("ss-{:x}-virtual-display-shutdown", record.id),
        None,
    );
    for turn in 0..8 {
        match pending.poll(send, turn) {
            rpc::Progress::Result(_) => {
                return pending.release_result(send);
            }
            rpc::Progress::Waiting => {}
            rpc::Progress::Failed | rpc::Progress::Unresolved => break,
        }
    }
    pending.shutdown(send)
}
