use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_SESSION_ID_BYTES: usize = 128;
pub const MAX_NAME_BYTES: usize = 128;
pub const MAX_PROTOCOL_BYTES: usize = 32;
pub const MAX_CODEC_BYTES: usize = 32;
pub const MAX_EVENT_TYPE_BYTES: usize = 64;
pub const MAX_EVENT_PAYLOAD_BYTES: usize = 4096;
pub const MAX_SESSION_LIMIT: usize = 25;
pub const MAX_ACTIVE_LIMIT: usize = 16;
pub const MAX_DETAIL_SAMPLES: usize = 32;
pub const MAX_DETAIL_EVENTS: usize = 8;
pub const MAX_STORED_SAMPLES: usize = 7200;
pub const MAX_STORED_EVENTS: usize = 2000;
pub const MAX_TIMELINE_PAGE_SAMPLES: usize = 32;
pub const MAX_TIMELINE_PAGE_EVENTS: usize = 8;
pub const MAX_TIMELINE_WINDOW_SAMPLES: usize = 2000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineKind {
    Samples,
    Events,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineCursor {
    pub session_id: String,
    pub kind: TimelineKind,
    pub first_id: String,
    pub last_id: String,
    pub after_id: Option<String>,
    pub total: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HistorySettings {
    pub enabled: bool,
    pub ttl_days: i32,
    pub db_size_limit_mb: i32,
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            ttl_days: 0,
            db_size_limit_mb: 0,
        }
    }
}

impl HistorySettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.ttl_days < 0 || self.db_size_limit_mb < 0 {
            return Err("history retention and size limits must be nonnegative integers");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMetadata {
    pub session_id: String,
    pub protocol: String,
    #[serde(default)]
    pub client_name: String,
    #[serde(default)]
    pub device_name: String,
    #[serde(default)]
    pub app_name: String,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub target_fps: u32,
    #[serde(default)]
    pub target_bitrate_kbps: u32,
    #[serde(default)]
    pub codec: String,
}

impl SessionMetadata {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_text(&self.session_id, 1, MAX_SESSION_ID_BYTES, "session_id")?;
        validate_text(&self.protocol, 1, MAX_PROTOCOL_BYTES, "protocol")?;
        validate_text(&self.client_name, 0, MAX_NAME_BYTES, "client_name")?;
        validate_text(&self.device_name, 0, MAX_NAME_BYTES, "device_name")?;
        validate_text(&self.app_name, 0, MAX_NAME_BYTES, "app_name")?;
        validate_text(&self.codec, 0, MAX_CODEC_BYTES, "codec")
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSample {
    pub session_id: String,
    pub recorded_at_unix: i64,
    #[serde(default)]
    pub frames_sent: u64,
    #[serde(default)]
    pub bytes_sent: u64,
    #[serde(default)]
    pub dropped_frames: u64,
    #[serde(default)]
    pub actual_fps: f64,
    #[serde(default)]
    pub actual_bitrate_kbps: f64,
    #[serde(default)]
    pub encode_latency_ms: Option<f64>,
}

impl SessionSample {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_text(&self.session_id, 1, MAX_SESSION_ID_BYTES, "session_id")?;
        for value in [self.actual_fps, self.actual_bitrate_kbps] {
            if !value.is_finite() || value < 0.0 {
                return Err("sample metrics must be finite and nonnegative");
            }
        }
        if self
            .encode_latency_ms
            .is_some_and(|value| !value.is_finite() || value < 0.0)
        {
            return Err("sample metrics must be finite and nonnegative");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEvent {
    pub session_id: String,
    pub recorded_at_unix: i64,
    pub event_type: String,
    #[serde(default)]
    pub payload: Option<Value>,
}

impl SessionEvent {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_text(&self.session_id, 1, MAX_SESSION_ID_BYTES, "session_id")?;
        validate_text(&self.event_type, 1, MAX_EVENT_TYPE_BYTES, "event_type")?;
        if self.payload.as_ref().is_some_and(|payload| {
            serde_json::to_vec(payload).map_or(true, |bytes| bytes.len() > MAX_EVENT_PAYLOAD_BYTES)
        }) {
            return Err("event payload exceeds the 4096 byte limit");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    SettingsGet {},
    SettingsUpdate {
        settings: HistorySettings,
        expected_revision: u64,
    },
    StreamActivityBegin {
        session_id: String,
    },
    StreamActivityEnd {
        session_id: String,
    },
    SessionBegin {
        metadata: SessionMetadata,
        #[serde(default)]
        started_at_unix: Option<i64>,
    },
    SessionEnd {
        session_id: String,
        #[serde(default)]
        ended_at_unix: Option<i64>,
    },
    RecordSample {
        sample: SessionSample,
    },
    RecordEvent {
        event: SessionEvent,
    },
    ListSessions {
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        offset: Option<usize>,
    },
    SessionDetail {
        session_id: String,
    },
    TimelineOpen {
        session_id: String,
        #[serde(default)]
        sample_limit: Option<usize>,
    },
    TimelinePage {
        session_id: String,
        kind: TimelineKind,
        cursor: TimelineCursor,
        #[serde(default)]
        limit: Option<usize>,
    },
    ActiveSessions {
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        offset: Option<usize>,
    },
    HistoryStatus {},
    DeleteSession {
        session_id: String,
    },
}

pub(crate) fn validate_text(
    value: &str,
    minimum_bytes: usize,
    maximum_bytes: usize,
    name: &'static str,
) -> Result<(), &'static str> {
    if value.len() < minimum_bytes
        || value.len() > maximum_bytes
        || value.chars().any(char::is_control)
    {
        return Err(match name {
            "session_id" => "session_id has invalid length or control characters",
            "protocol" => "protocol has invalid length or control characters",
            "client_name" => "client_name has invalid length or control characters",
            "device_name" => "device_name has invalid length or control characters",
            "app_name" => "app_name has invalid length or control characters",
            "codec" => "codec has invalid length or control characters",
            "event_type" => "event_type has invalid length or control characters",
            _ => "text field has invalid length or control characters",
        });
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub protocol: String,
    pub client_name: String,
    pub device_name: String,
    pub app_name: String,
    pub width: u32,
    pub height: u32,
    pub target_fps: u32,
    pub target_bitrate_kbps: u32,
    pub codec: String,
    pub started_at_unix: i64,
    pub ended_at_unix: Option<i64>,
    pub duration_seconds: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionDetail {
    pub session: SessionSummary,
    pub sample_count: usize,
    pub event_count: usize,
    pub samples_truncated: bool,
    pub events_truncated: bool,
    pub samples: Vec<SessionSample>,
    pub events: Vec<SessionEvent>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PruneReport {
    pub sessions_deleted: usize,
    pub live_bytes: u64,
    pub size_limit_bytes: u64,
    pub size_limit_unmet: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct HistoryStatus {
    pub available: bool,
    pub configuration_ready: bool,
    pub applied_revision: Option<u64>,
    pub enabled: bool,
    pub effective_enabled: bool,
    pub degraded: bool,
    pub failed_operations: u64,
    pub active_streams: usize,
    pub pending_activities: usize,
    pub sessions: usize,
    pub samples: usize,
    pub events: usize,
    pub live_bytes: u64,
    pub last_prune: PruneReport,
}
