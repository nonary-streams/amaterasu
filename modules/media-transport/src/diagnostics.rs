use amaterasu_module_sdk::diagnostics::LatencyMetric;
use serde::Serialize;

/// Queue residence ends when the network provider accepts a datagram locally.
/// It is not NIC completion, remote receipt, or client presentation.
#[derive(Default, Serialize)]
pub(crate) struct DeliveryTiming {
    pub video_queue_wait_us: LatencyMetric,
    pub audio_queue_wait_us: LatencyMetric,
    pub queue_high_water_datagrams: usize,
    pub queue_high_water_bytes: usize,
    pub send_retries: u64,
    pub partial_batch_sends: u64,
}
