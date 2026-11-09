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

/// Starts the shared input flow for every authenticated message: when its
/// datagram reached the kernel and when it was queued for the consumer. The
/// consumer continues the same flow from the stream handle and sequence both
/// sides already exchange.
#[derive(Clone, Copy)]
pub(crate) struct FlowTrace {
    tracer: amaterasu_module_sdk::trace::Tracer,
    track: amaterasu_module_sdk::trace::Track,
    arrived: amaterasu_module_sdk::trace::Stage,
    authenticated: amaterasu_module_sdk::trace::Stage,
}

impl FlowTrace {
    pub fn new(tracer: amaterasu_module_sdk::trace::Tracer) -> Self {
        use amaterasu_module_sdk::trace::names;
        Self {
            track: tracer.track(names::INPUT_TRACK),
            arrived: tracer.stage(names::SOCKET_ARRIVED),
            authenticated: tracer.stage(names::CONTROL_AUTHENTICATED),
            tracer,
        }
    }

    /// The arrival time of a datagram that waited `kernel_residence_us` in
    /// the socket before this read, or the read time when that is unknown.
    pub fn arrival(
        &self,
        kernel_residence_us: Option<u64>,
    ) -> amaterasu_module_sdk::trace::TraceTime {
        self.tracer
            .now()
            .earlier_by(std::time::Duration::from_micros(
                kernel_residence_us.unwrap_or(0),
            ))
    }

    pub fn authenticated(
        &self,
        stream: u64,
        sequence: u64,
        arrived: Option<amaterasu_module_sdk::trace::TraceTime>,
    ) {
        let flow = amaterasu_module_sdk::trace::flow_id(stream, sequence);
        if let Some(arrived) = arrived {
            self.tracer.mark_at(self.track, flow, self.arrived, arrived);
        }
        self.tracer.mark(self.track, flow, self.authenticated);
    }
}
