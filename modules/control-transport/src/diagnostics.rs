use amaterasu_module_sdk::diagnostics::LatencyMetric;
use serde::Serialize;

/// Decryption attempts and authenticated queue observations. Never retain input
/// contents or session secrets.
#[derive(Default, Serialize)]
pub(crate) struct ControlTiming {
    pub socket_receive_us: LatencyMetric,
    pub kernel_residence_us: LatencyMetric,
    pub datagram_arrival_gap_us: LatencyMetric,
    pub receive_budget_exhausted: u64,
    pub kernel_timing_unknown: u64,
    pub socket_send_retries: u64,
    pub decrypt_us: LatencyMetric,
    pub queue_wait_us: LatencyMetric,
    pub consumer_callback_us: LatencyMetric,
    pub tick_gap_us: LatencyMetric,
    pub queue_high_water: usize,
    pub consumer_retries: u64,
}
