//! A capability required by participating failure-driven stop boundaries.
use super::StreamIssue;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// A locally observed condition, with no authority over a stream.
#[derive(Clone, Debug)]
pub struct StreamFailure {
    code: &'static str,
    reason: &'static str,
    evidence: StreamFailureEffect,
    fallback: Option<&'static str>,
}

impl StreamFailure {
    pub fn requirement_failed(code: &'static str) -> Self {
        Self {
            code,
            reason: code,
            evidence: StreamFailureEffect::RequirementFailed,
            fallback: None,
        }
    }

    pub fn request_unchanged(code: &'static str) -> Self {
        Self {
            code,
            reason: code,
            evidence: StreamFailureEffect::RequestUnchanged,
            fallback: None,
        }
    }

    pub fn fallback_applied(code: &'static str, fallback: &'static str) -> Self {
        Self {
            code,
            reason: code,
            evidence: StreamFailureEffect::FallbackApplied,
            fallback: Some(fallback),
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
    ) -> StreamFailureReport {
        // Reuse the bounded diagnostic shape. This never carries a decision.
        let issue = StreamIssue::advisory(operation, layer, phase, self.code).because(self.reason);
        StreamFailureReport {
            issue,
            evidence: self.evidence,
            fallback: self.fallback,
        }
    }
}

/// Facts contextualized at their owner. Reporters cannot supply a disposition.
pub struct StreamFailureReport {
    issue: StreamIssue,
    evidence: StreamFailureEffect,
    fallback: Option<&'static str>,
}

impl StreamFailureReport {
    pub fn because(mut self, reason: impl Into<String>) -> Self {
        self.issue.reason = reason.into();
        self
    }

    pub fn caused_by(mut self, layer: super::LifecycleLayer) -> Self {
        self.issue.caused_by = Some(layer);
        self
    }

    pub fn with_phase(mut self, phase: impl Into<String>) -> Self {
        self.issue.phase = phase.into();
        self
    }

    pub fn with_code(mut self, code: &'static str) -> Self {
        self.issue.code = code.into();
        self
    }

    pub fn requires_containment(&self) -> bool {
        self.evidence.requires_containment()
    }

    /// Diagnostic attribution for local containment, request-only refusal or
    /// an already applied fallback. An issue never grants terminal authority.
    pub fn into_diagnostic(self) -> StreamIssue {
        self.classified(StreamFailureAction::RefuseRequest)
    }

    fn classified(self, action: StreamFailureAction) -> StreamIssue {
        let mut issue = self.issue;
        issue.fallback = self.fallback.map(str::to_owned);
        issue.disposition = if self.evidence.requires_containment()
            || (self.evidence == StreamFailureEffect::PreparationUnchanged
                && action == StreamFailureAction::EndStream)
        {
            super::IssueDisposition::CriticalFailure
        } else if issue.fallback.is_some() {
            super::IssueDisposition::DemotedWarning
        } else {
            super::IssueDisposition::AdvisoryWarning
        };
        issue
    }
}

/// Failure evidence reported by a provider. The reporting contract cannot
/// express a decision to keep or terminate its caller's stream.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct StreamFailureFact {
    pub operation: String,
    pub layer: super::LifecycleLayer,
    pub phase: String,
    pub code: String,
    pub reason: String,
    pub site: String,
}

impl From<&StreamIssue> for StreamFailureFact {
    fn from(issue: &StreamIssue) -> Self {
        Self {
            operation: issue.operation.clone(),
            layer: issue.layer,
            phase: issue.phase.clone(),
            code: issue.code.clone(),
            reason: issue.reason.clone(),
            site: issue.site.clone(),
        }
    }
}

/// The owning consumer establishes this from its authenticated operation and
/// its provider's current result. It is deliberately absent from the report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamFailureEffect {
    PreparationUnchanged,
    RequiredStartUnverified,
    RequirementFailed,
    RequestUnchanged,
    FallbackApplied,
}

impl StreamFailureEffect {
    fn requires_containment(self) -> bool {
        matches!(
            self,
            Self::RequiredStartUnverified | Self::RequirementFailed
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamFailureAction {
    RefuseRequest,
    EndStream,
}

pub trait StreamFailurePolicy: Send + Sync {
    fn action(&self, fact: &StreamFailureFact, effect: StreamFailureEffect) -> StreamFailureAction;
}

pub struct StandardStreamFailurePolicy;

impl StreamFailurePolicy for StandardStreamFailurePolicy {
    fn action(&self, _: &StreamFailureFact, effect: StreamFailureEffect) -> StreamFailureAction {
        if effect.requires_containment() {
            StreamFailureAction::EndStream
        } else {
            StreamFailureAction::RefuseRequest
        }
    }
}

/// Identity of a concrete operation instance, independent of its diagnostic name.
#[derive(Clone, Debug)]
pub struct StreamFailureScope {
    operation: Arc<str>,
    owner: Arc<()>,
    instance: Arc<()>,
    live_action: Option<Arc<AtomicBool>>,
}

impl StreamFailureScope {
    pub fn new(operation: impl Into<String>) -> Self {
        let owner = Arc::new(());
        Self {
            operation: Arc::from(operation.into()),
            instance: owner.clone(),
            owner,
            live_action: None,
        }
    }

    /// Create one pending action under the same terminal owner.
    pub fn action(&self) -> Self {
        Self {
            operation: self.operation.clone(),
            owner: self.owner.clone(),
            instance: Arc::new(()),
            live_action: Some(Arc::new(AtomicBool::new(true))),
        }
    }

    /// Retire a pending action on successful completion, refusal or removal.
    pub fn invalidate_action(&self) {
        if let Some(live) = &self.live_action {
            live.store(false, Ordering::Release);
        }
    }

    pub fn is_owner(&self) -> bool {
        Arc::ptr_eq(&self.instance, &self.owner)
    }

    pub fn operation(&self) -> &str {
        &self.operation
    }

    fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.instance, &other.instance)
            && self
                .live_action
                .as_ref()
                .is_none_or(|live| live.load(Ordering::Acquire))
    }

    fn owns(&self, other: &Self) -> bool {
        self.is_owner() && Arc::ptr_eq(&self.owner, &other.owner)
    }
}

/// Proof that the common handler accepted a nonterminal report.
#[derive(Debug)]
pub struct StreamContinuation {
    issue: StreamIssue,
    scope: StreamFailureScope,
}

impl StreamContinuation {
    pub fn into_issue(self) -> StreamIssue {
        self.issue
    }

    pub fn matches_scope(&self, scope: &StreamFailureScope) -> bool {
        self.scope.matches(scope)
    }
}

/// Proof of an explicitly critical, valid failure with a nonempty reason.
/// Fields are private: advisory/demoted reports cannot create this capability.
/// This gates participating callers; native code may still bypass this API.
#[derive(Debug)]
pub struct StreamTermination {
    issue: StreamIssue,
    scope: StreamFailureScope,
}

/// The final directive from the common failure handler. Lower layers report
/// facts; participating transport and lifecycle boundaries execute
/// this directive instead of inventing another rejection policy.
#[derive(Debug)]
pub enum StreamFailureDecision {
    Continue(StreamContinuation),
    Terminate(StreamTermination),
    Invalid(StreamIssue),
}

/// One decision point shared by participating stream and wire boundaries.
/// This is synchronous policy, not the asynchronous diagnostic event store.
/// It does not establish native-code isolation or enforce unmigrated callers.
pub struct StreamFailureHandler;

impl StreamFailureHandler {
    /// Decide one scoped failure at its owner. A policy may reject a request
    /// without ending a verified preparation, but cannot authorize a retry
    /// across an unverified native start.
    #[track_caller]
    pub fn adjudicate(
        policy: &dyn StreamFailurePolicy,
        fact: StreamFailureFact,
        effect: StreamFailureEffect,
        scope: &StreamFailureScope,
        owner: super::LifecycleLayer,
        owner_code: &'static str,
    ) -> StreamFailureDecision {
        let mut evidence =
            StreamIssue::advisory(&fact.operation, fact.layer, &fact.phase, &fact.code)
                .because(&fact.reason);
        evidence.site.clone_from(&fact.site);
        if !evidence.valid()
            || evidence.reason.is_empty()
            || evidence.operation != scope.operation.as_ref()
        {
            return StreamFailureDecision::Invalid(evidence);
        }
        let issue = StreamIssue::advisory(&fact.operation, owner, &fact.phase, owner_code)
            .because(&fact.code)
            .caused_by(fact.layer);
        let report = StreamFailureReport {
            issue,
            evidence: effect,
            fallback: None,
        };
        Self::issue_decision(report, policy.action(&fact, effect), scope)
    }

    /// Decide a local report after its owner supplies the operation instance.
    ///
    /// ```compile_fail
    /// use amaterasu_module_sdk::stream_lifecycle::{
    ///     LifecycleLayer, StandardStreamFailurePolicy, StreamFailureHandler, StreamFailureScope,
    ///     StreamIssue,
    /// };
    /// let issue = StreamIssue::critical("stream", LifecycleLayer::Capture, "open", "device_lost");
    /// let scope = StreamFailureScope::new("stream");
    /// StreamFailureHandler::adjudicate_report(&StandardStreamFailurePolicy, issue, &scope);
    /// ```
    pub fn adjudicate_report(
        policy: &dyn StreamFailurePolicy,
        report: StreamFailureReport,
        scope: &StreamFailureScope,
    ) -> StreamFailureDecision {
        let fact = StreamFailureFact::from(&report.issue);
        if !report.issue.valid() || fact.reason.is_empty() || fact.operation != scope.operation() {
            return StreamFailureDecision::Invalid(report.issue);
        }
        let action = policy.action(&fact, report.evidence);
        Self::issue_decision(report, action, scope)
    }

    fn issue_decision(
        report: StreamFailureReport,
        action: StreamFailureAction,
        scope: &StreamFailureScope,
    ) -> StreamFailureDecision {
        let issue = report.classified(action);
        if !issue.valid() {
            return StreamFailureDecision::Invalid(issue);
        }
        if issue.is_critical() {
            StreamFailureDecision::Terminate(StreamTermination {
                issue,
                scope: scope.clone(),
            })
        } else {
            StreamFailureDecision::Continue(StreamContinuation {
                issue,
                scope: scope.clone(),
            })
        }
    }
}

impl StreamTermination {
    pub fn issue(&self) -> &StreamIssue {
        &self.issue
    }

    pub fn matches_scope(&self, scope: &StreamFailureScope) -> bool {
        self.scope.matches(scope)
    }

    /// Apply an effect at the exact action that the policy adjudicated.
    pub fn authorize<T>(
        &self,
        scope: &StreamFailureScope,
        effect: impl FnOnce(&StreamIssue) -> T,
    ) -> Option<T> {
        self.matches_scope(scope).then(|| effect(&self.issue))
    }

    /// Hand the decision to its owner after verifying the current action.
    /// Callers must supply the action that still owns the pending completion.
    pub fn settle_action(
        mut self,
        action: &StreamFailureScope,
        owner: &StreamFailureScope,
    ) -> Option<Self> {
        if !self.matches_scope(action)
            || !owner.owns(action)
            || !action.live_action.as_ref()?.swap(false, Ordering::AcqRel)
        {
            return None;
        }
        self.scope = owner.clone();
        Some(self)
    }

    /// Consume a decision at its exact owner. Pending actions must settle first.
    pub fn terminate<T>(
        self,
        owner: &StreamFailureScope,
        effect: impl FnOnce(StreamIssue) -> T,
    ) -> Option<T> {
        (owner.is_owner() && self.matches_scope(owner)).then(|| effect(self.issue))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream_lifecycle::LifecycleLayer;

    #[test]
    fn a_decision_belongs_to_its_action_and_owner_instance() {
        let prior = StreamFailureScope::new("stream");
        let current = StreamFailureScope::new("stream");
        let action = prior.action();
        let report = StreamFailure::requirement_failed("device_unavailable").at(
            "stream",
            LifecycleLayer::Capture,
            "open",
        );
        let StreamFailureDecision::Terminate(permit) =
            StreamFailureHandler::adjudicate_report(&StandardStreamFailurePolicy, report, &action)
        else {
            panic!("required capture failed");
        };
        assert_eq!(permit.authorize(&action, |_| 1), Some(1));
        assert_eq!(permit.authorize(&prior.action(), |_| 1), None);
        assert_eq!(permit.terminate(&current, |_| unreachable!()), None::<()>);
    }

    #[test]
    fn reflected_diagnostics_and_invalid_facts_confer_no_authority() {
        let scope = StreamFailureScope::new("stream");
        for fact in [
            StreamFailure::request_unchanged("refused").at("stream", LifecycleLayer::Input, "send"),
            StreamFailure::fallback_applied("unavailable", "skip").at(
                "stream",
                LifecycleLayer::Input,
                "send",
            ),
            StreamFailure::requirement_failed("bad payload!").at(
                "stream",
                LifecycleLayer::Input,
                "send",
            ),
            StreamFailure::requirement_failed("unavailable").at(
                "other",
                LifecycleLayer::Input,
                "send",
            ),
        ] {
            assert!(!matches!(
                StreamFailureHandler::adjudicate_report(&StandardStreamFailurePolicy, fact, &scope),
                StreamFailureDecision::Terminate(_)
            ));
        }
    }
}

#[cfg(test)]
mod authority_tests {
    use super::*;
    use crate::stream_lifecycle::LifecycleLayer;

    fn decide(scope: &StreamFailureScope) -> StreamTermination {
        let failure = StreamFailure::requirement_failed("device_lost").at(
            scope.operation(),
            LifecycleLayer::Capture,
            "deliver",
        );
        let StreamFailureDecision::Terminate(permit) =
            StreamFailureHandler::adjudicate_report(&StandardStreamFailurePolicy, failure, scope)
        else {
            panic!("required capture was lost");
        };
        permit
    }

    #[test]
    fn only_a_live_exact_action_can_transfer_its_decision() {
        let owner = StreamFailureScope::new("stream");
        let successor = StreamFailureScope::new("stream");
        let action = owner.action();
        let another = owner.action();
        assert!(decide(&action).terminate(&owner, |_| ()).is_none());
        assert!(decide(&action).settle_action(&another, &owner).is_none());
        assert!(decide(&action).settle_action(&action, &successor).is_none());
        let stale = decide(&action);
        action.invalidate_action();
        assert!(stale.settle_action(&action, &owner).is_none());
        let first = decide(&another);
        let duplicate = decide(&another);
        let settled = first.settle_action(&another, &owner).expect("live action");
        assert!(duplicate.settle_action(&another, &owner).is_none());
        assert_eq!(
            settled.terminate(&owner, |issue| issue.code).as_deref(),
            Some("device_lost")
        );
    }

    #[test]
    fn owner_policy_receives_original_provider_fact() {
        struct Strict(StreamFailureFact);
        impl StreamFailurePolicy for Strict {
            fn action(
                &self,
                actual: &StreamFailureFact,
                _: StreamFailureEffect,
            ) -> StreamFailureAction {
                assert_eq!(actual, &self.0);
                StreamFailureAction::RefuseRequest
            }
        }
        let scope = StreamFailureScope::new("stream");
        let issue = StreamIssue::advisory(
            "stream",
            LifecycleLayer::EncodeVideo,
            "start",
            "provider_code",
        )
        .because("selected_format_unavailable");
        let fact = StreamFailureFact::from(&issue);
        let decision = StreamFailureHandler::adjudicate(
            &Strict(fact.clone()),
            fact,
            StreamFailureEffect::PreparationUnchanged,
            &scope,
            LifecycleLayer::Rtsp,
            "owner_code",
        );
        let StreamFailureDecision::Continue(continuation) = decision else {
            panic!("unchanged preparation");
        };
        let issue = continuation.into_issue();
        assert_eq!(issue.layer, LifecycleLayer::Rtsp);
        assert_eq!(issue.caused_by, Some(LifecycleLayer::EncodeVideo));
    }
}
