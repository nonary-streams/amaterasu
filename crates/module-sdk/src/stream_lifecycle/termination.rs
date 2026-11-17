//! A capability required by participating failure-driven stop boundaries.
use super::StreamIssue;

/// A lower-layer failure report with a stable local code. Unlike a string,
/// this makes its criticality/fallback explicit before it reaches a boundary.
#[derive(Clone, Debug)]
pub struct StreamFailure {
    code: &'static str,
    reason: &'static str,
    disposition: super::IssueDisposition,
    fallback: Option<&'static str>,
    reported: Option<StreamIssue>,
}

impl StreamFailure {
    pub fn critical(code: &'static str) -> Self {
        Self {
            code,
            reason: code,
            disposition: super::IssueDisposition::CriticalFailure,
            fallback: None,
            reported: None,
        }
    }
    pub fn advisory(code: &'static str) -> Self {
        Self {
            code,
            reason: code,
            disposition: super::IssueDisposition::AdvisoryWarning,
            fallback: None,
            reported: None,
        }
    }
    pub fn demoted(code: &'static str, fallback: &'static str) -> Self {
        Self {
            code,
            reason: code,
            disposition: super::IssueDisposition::DemotedWarning,
            fallback: Some(fallback),
            reported: None,
        }
    }
    /// Preserve a dependency's original classification at a caller boundary;
    /// a failed RPC must not silently promote its warning into a critical error.
    pub fn reported(local_code: &'static str, report: StreamIssue) -> Self {
        Self {
            code: local_code,
            reason: local_code,
            disposition: report.disposition,
            fallback: None,
            reported: Some(report),
        }
    }

    pub fn because(mut self, reason: &'static str) -> Self {
        self.reason = reason;
        self
    }
    pub fn code(&self) -> &'static str {
        self.code
    }
    #[track_caller]
    pub fn at(
        self,
        operation: impl Into<String>,
        layer: super::LifecycleLayer,
        phase: impl Into<String>,
    ) -> StreamIssue {
        let operation = operation.into();
        let phase = phase.into();
        if let Some(mut report) = self.reported {
            if !report.valid() || report.reason.is_empty() {
                return report;
            }
            let dependency = report.layer;
            let reason = report.code.clone();
            report.operation = operation;
            report.layer = layer;
            report.phase = phase;
            report.code = self.code.into();
            report.reason = reason;
            report.caused_by = Some(dependency);
            return report;
        }
        let mut issue = match self.disposition {
            super::IssueDisposition::CriticalFailure => {
                StreamIssue::critical(operation, layer, phase, self.code)
            }
            super::IssueDisposition::AdvisoryWarning => {
                StreamIssue::advisory(operation, layer, phase, self.code)
            }
            super::IssueDisposition::DemotedWarning => StreamIssue::demoted(
                operation,
                layer,
                phase,
                self.code,
                self.fallback
                    .expect("demotion constructor supplies fallback"),
            ),
        };
        issue.reason = self.reason.into();
        issue
    }
}

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

    /// Authorize several effects belonging to this same reported failure.
    /// The capability is immutable; it does not authorize another stream's
    /// authentication, ownership, media substitution or native retirement.
    pub fn authorize<T>(&self, effect: impl FnOnce(&StreamIssue) -> T) -> T {
        effect(&self.issue)
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
