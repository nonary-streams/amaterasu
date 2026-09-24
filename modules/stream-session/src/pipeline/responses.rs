use super::*;

pub(super) struct MediaStatus {
    pub ready: bool,
    pub peer_discovery: bool,
    pub video_port: Option<u16>,
    pub audio_port: Option<u16>,
}

pub(super) fn allocation_id(record: &Record, resource: Resource, payload: &Value) -> Option<u64> {
    if payload["ok"] != true {
        return None;
    }
    if resource == Resource::Session {
        session_response(record, payload)?["id"]
            .as_u64()
            .filter(|id| *id != 0)
    } else {
        payload["stream"].as_u64().filter(|id| *id != 0)
    }
}

pub(super) fn session_response<'a>(record: &Record, payload: &'a Value) -> Option<&'a Value> {
    let values = payload["sessions"].as_array()?;
    if values.len() != 1 {
        return None;
    }
    let value = &values[0];
    (payload["ok"] == true
        && value["key"] == record.operation(Resource::Session)
        && record
            .resources
            .get(&Resource::Session)
            .is_none_or(|id| value["id"] == *id))
    .then_some(value)
}

pub(super) fn require_lease(
    record: &Record,
    payload: &Value,
    audio: bool,
) -> Result<(), &'static str> {
    let session = session_response(record, payload).ok_or("invalid_session_response")?;
    if session["leases"][record.lease(audio)] != "shared" {
        return Err("lease_unconfirmed");
    }
    Ok(())
}

pub(super) fn component_status(payload: &Value, id: u64) -> Option<&Value> {
    if payload["stream"] == id {
        return Some(payload);
    }
    payload["streams"]
        .as_array()?
        .iter()
        .find(|value| value["stream"] == id)
}

pub(super) fn local_address(payload: &Value, name: &str) -> Result<String, &'static str> {
    let value = payload[name].as_str().ok_or("invalid_local_address")?;
    let address: std::net::SocketAddr = value.parse().map_err(|_| "invalid_local_address")?;
    if address.port() == 0 {
        return Err("invalid_local_address");
    }
    Ok(value.to_owned())
}

pub(super) fn media_status(record: &Record, payload: &Value) -> Result<MediaStatus, &'static str> {
    let config = &record.config;
    let expected_discovery = config.discovery.is_some();
    let peer_discovery = response_bool(
        payload,
        "peer_discovery",
        (!expected_discovery).then_some(false),
    )?;
    let ready = response_bool(payload, "ready", (!expected_discovery).then_some(true))?;
    let video_port = optional_port(&payload["video_port"])?;
    let audio_port = optional_port(&payload["audio_port"])?;
    if payload["peer_ip"] != json!(config.peer_ip)
        || payload["stream_audio"] != config.stream_audio
        || peer_discovery != expected_discovery
        || payload["video"] != json!(config.video)
        || payload["audio"]["packet_ms"] != config.audio.packet_ms
        || payload["audio"]["channels"] != 2
        || payload["audio"]["wire_payload_bytes"] != record.wire_audio_bytes
        || payload["video_encrypted"] != config.encrypt_video
        || payload["audio_encrypted"] != (config.stream_audio && config.encrypt_audio)
        || payload["permit_legacy_audio_iv_wrap"]
            != (config.stream_audio && config.permit_legacy_audio_iv_wrap)
    {
        return Err("invalid_transport_response");
    }
    if expected_discovery {
        if ready != (video_port.is_some() && audio_port.is_some()) {
            return Err("invalid_transport_response");
        }
    } else if !ready || video_port != config.video_port || audio_port != config.audio_port {
        return Err("invalid_transport_response");
    }
    Ok(MediaStatus {
        ready,
        peer_discovery,
        video_port,
        audio_port,
    })
}

fn response_bool(
    payload: &Value,
    field: &str,
    missing: Option<bool>,
) -> Result<bool, &'static str> {
    match payload.as_object().and_then(|object| object.get(field)) {
        Some(Value::Bool(value)) => Ok(*value),
        None => missing.ok_or("invalid_transport_response"),
        Some(_) => Err("invalid_transport_response"),
    }
}

fn optional_port(value: &Value) -> Result<Option<u16>, &'static str> {
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_u64()
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port != 0)
        .map(Some)
        .ok_or("invalid_transport_response")
}

pub(super) fn missing(payload: &Value) -> bool {
    matches!(
        payload["code"].as_str(),
        Some("not_found" | "stream_not_found" | "stream_closed" | "session_not_found")
    )
}
