//! Capture-only publishing and notifications. No encoding or provider selection
//! runs on a capture thread. Each channel represents one capture session.
mod publisher;
mod subscription;

pub use publisher::{CaptureFrames, CapturePublisher, PublishError, capture_channel};
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
