//! A capability required by participating failure-driven stop boundaries.
use super::StreamIssue;

/// Proof of an explicitly critical, valid failure with a nonempty reason.
/// Fields are private: advisory/demoted reports cannot create this capability.
/// This gates participating callers; native code may still bypass this API.
#[derive(Debug)]
pub struct StreamTermination {
    issue: StreamIssue,
}

/// The final directive from the common failure handler. Lower layers report
/// facts/disposition; participating transport and lifecycle boundaries execute
/// this directive instead of inventing another rejection policy.
#[derive(Debug)]
pub enum StreamFailureDecision {
    Continue(StreamIssue),
    Terminate(StreamTermination),
    Invalid(StreamIssue),
}

/// One decision point shared by participating stream and wire boundaries.
/// This is synchronous policy, not the asynchronous diagnostic event store.
/// It does not establish native-code isolation or enforce unmigrated callers.
pub struct StreamFailureHandler;

impl StreamFailureHandler {
    pub fn decide(issue: StreamIssue) -> StreamFailureDecision {
        if !issue.valid() || issue.reason.is_empty() {
            return StreamFailureDecision::Invalid(issue);
        }
        if issue.is_critical() {
            StreamFailureDecision::Terminate(StreamTermination { issue })
        } else {
            StreamFailureDecision::Continue(issue)
        }
    }
}

impl TryFrom<StreamIssue> for StreamTermination {
    type Error = StreamIssue;

    fn try_from(issue: StreamIssue) -> Result<Self, Self::Error> {
        match StreamFailureHandler::decide(issue) {
            StreamFailureDecision::Terminate(permit) => Ok(permit),
            StreamFailureDecision::Continue(issue) | StreamFailureDecision::Invalid(issue) => {
                Err(issue)
            }
        }
    }
}

impl StreamTermination {
    pub fn issue(&self) -> &StreamIssue {
        &self.issue
    }

    /// Run a terminal effect only after this capability was established.
    pub fn terminate<T>(self, effect: impl FnOnce(StreamIssue) -> T) -> T {
        effect(self.issue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream_lifecycle::LifecycleLayer;

    #[test]
    fn only_explicit_valid_critical_failure_with_reason_can_run_a_terminal_effect() {
        let issue = StreamIssue::critical("stream", LifecycleLayer::Capture, "open", "unavailable")
            .because("device_missing");
        let mut effects = 0;
        StreamTermination::try_from(issue)
            .unwrap()
            .terminate(|failure| {
                assert_eq!(failure.reason, "device_missing");
                effects += 1;
            });
        assert_eq!(effects, 1);
        let mut no_reason =
            StreamIssue::critical("stream", LifecycleLayer::Capture, "open", "failed");
        no_reason.reason.clear();
        let mut invalid =
            StreamIssue::critical("stream", LifecycleLayer::Capture, "open", "failed")
                .because("reason");
        invalid.code = "secret payload!".into();
        for issue in [
            StreamIssue::advisory("stream", LifecycleLayer::Input, "feedback", "unavailable")
                .because("provider_missing"),
            StreamIssue::demoted(
                "stream",
                LifecycleLayer::Input,
                "feedback",
                "unavailable",
                "skip_feedback",
            )
            .because("provider_missing"),
            no_reason,
            invalid,
        ] {
            if let Ok(permit) = StreamTermination::try_from(issue) {
                permit.terminate(|_| effects += 1);
            }
        }
        assert_eq!(
            effects, 1,
            "warnings or invalid reports ran a terminal effect"
        );
    }
}
