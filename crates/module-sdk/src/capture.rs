//! Capture-only publishing and notifications. No encoding or provider selection
//! runs on a capture thread. Each channel represents one capture session.
mod connection;
mod negotiation;
mod provenance;
mod publisher;
mod selector;
mod subscription;

pub use connection::{ConnectionError, ResourceConnection};
pub use negotiation::{
    Accessibility, DeliveryFormat, DeliveryRequirement, DeliverySelection,
    MAX_DELIVERY_CAPABILITY_BYTES, MAX_DELIVERY_OFFERS, MAX_DELIVERY_REQUIREMENTS,
    NegotiationError, Ownership, ProviderOffer, Schema, select_delivery,
    select_delivery_with_compatibility,
};
pub use provenance::color_provenance;
pub use publisher::{CaptureFrames, CapturePublisher, PublishError, capture_channel};
pub use selector::{EXACT_TARGET_PREFIX, exact_target_key, exact_target_selector};
pub use subscription::{CaptureNotice, CaptureNotifications, CaptureSubscription};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwapRequest {
    pub preferred_provider: Option<String>,
    pub output: Option<String>,
    pub reason: String,
}

impl SwapRequest {
    pub fn validate(&self) -> Result<(), CaptureError> {
        if !valid_code(&self.reason)
            || self
                .preferred_provider
                .as_deref()
                .is_some_and(|value| !valid_code(value))
            || self.output.as_deref().is_some_and(|value| {
                value.is_empty() || value.len() > 512 || value.chars().any(char::is_control)
            })
        {
            return Err(CaptureError::InvalidRequest);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingSwap {
    pub id: u64,
    pub request: SwapRequest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SwapDecision {
    Accepted,
    Rejected { reason: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureError {
    Closed,
    Busy,
    InvalidRequest,
    Unsupported,
    ProviderFailed,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Closed => "capture is closed",
            Self::Busy => "capture mailbox is busy",
            Self::InvalidRequest => "invalid capture request",
            Self::Unsupported => "capture notifications are unsupported",
            Self::ProviderFailed => "capture provider failed",
        })
    }
}

impl std::error::Error for CaptureError {}

fn valid_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

#[cfg(test)]
mod tests;
