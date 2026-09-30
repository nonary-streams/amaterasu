use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub poll_interval_ms: u32,
    pub history_retention_seconds: u32,
    pub max_history_points: u32,
    pub pause_when_hidden: bool,
    pub show_active_sessions: bool,
    pub show_host_stats: bool,
    pub show_host_charts: bool,
    pub show_session_history: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_ms: 2_000,
            history_retention_seconds: 300,
            max_history_points: 300,
            pause_when_hidden: true,
            show_active_sessions: true,
            show_host_stats: true,
            show_host_charts: true,
            show_session_history: true,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(250..=60_000).contains(&self.poll_interval_ms) {
            return Err("poll_interval_ms must be between 250 and 60000");
        }
        if !(30..=3_600).contains(&self.history_retention_seconds) {
            return Err("history_retention_seconds must be between 30 and 3600");
        }
        if !(30..=2_000).contains(&self.max_history_points) {
            return Err("max_history_points must be between 30 and 2000");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostSnapshot {
    pub cpu_percent: Option<f64>,
    pub cpu_temp_c: Option<f64>,
    pub ram_used_bytes: Option<u64>,
    pub ram_total_bytes: Option<u64>,
    pub gpu_percent: Option<f64>,
    pub gpu_encoder_percent: Option<f64>,
    pub gpu_temp_c: Option<f64>,
    pub vram_used_bytes: Option<u64>,
    pub vram_total_bytes: Option<u64>,
    pub net_rx_bps: Option<f64>,
    pub net_tx_bps: Option<f64>,
}

impl HostSnapshot {
    pub fn unavailable() -> Self {
        Self {
            cpu_percent: None,
            cpu_temp_c: None,
            ram_used_bytes: None,
            ram_total_bytes: None,
            gpu_percent: None,
            gpu_encoder_percent: None,
            gpu_temp_c: None,
            vram_used_bytes: None,
            vram_total_bytes: None,
            net_rx_bps: None,
            net_tx_bps: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    pub recorded_at_unix_ms: i64,
    pub host: HostSnapshot,
}
