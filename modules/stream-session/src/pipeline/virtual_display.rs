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
                .filter(|output| is_kwin_virtual_output(output))
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
/// KWin names virtual outputs `Virtual-N`. Connector numbers keep counting
/// across driver reloads, so any canonical index up to 64 is accepted.
fn is_kwin_virtual_output(output: &str) -> bool {
    output
        .strip_prefix("kwin:Virtual-")
        .filter(|index| !index.starts_with('0') && index.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|index| index.parse::<u8>().ok())
        .is_some_and(|index| (1..=64).contains(&index))
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

#[cfg(test)]
mod tests {
    use super::is_kwin_virtual_output;

    #[test]
    fn kwin_virtual_outputs_accept_any_canonical_index_up_to_sixty_four() {
        for accepted in [
            "kwin:Virtual-1",
            "kwin:Virtual-8",
            "kwin:Virtual-9",
            "kwin:Virtual-16",
            "kwin:Virtual-64",
        ] {
            assert!(is_kwin_virtual_output(accepted), "{accepted}");
        }
        for rejected in [
            "kwin:Virtual-0",
            "kwin:Virtual-65",
            "kwin:Virtual-09",
            "kwin:Virtual-+9",
            "kwin:Virtual-",
            "kwin:Virtual-9 ",
            "kwin:Virtual-256",
            "kwin:DP-1",
            "Virtual-9",
        ] {
            assert!(!is_kwin_virtual_output(rejected), "{rejected}");
        }
    }
}
